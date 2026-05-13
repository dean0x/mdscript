---
feature: mds-compiler
name: MDS Compiler
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, or debugging output rendering. Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, export, include, define, for, if, closure, lexical scope, prompt export, nested function calls, arg parsing, warnings, quiet mode, stdin."
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
created: 2026-05-12
updated: 2026-05-13
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
| `compile_str(source)` | Compile from string, no options |
| `compile_str_with(source, base_dir, runtime_vars)` | Compile from string with options |
| `compile_collecting_warnings(path, runtime_vars)` | Compile and return `(String, Vec<String>)` — caller controls warning output |
| `compile_str_collecting_warnings(source, base_dir, runtime_vars)` | String variant of the above |
| `check(path, runtime_vars)` | Validate a file without rendering |
| `check_str(source)` | Validate from string, no options |
| `check_str_with(source, base_dir, runtime_vars)` | Validate from string with options |
| `load_vars_file(path)` | Load runtime vars from a JSON file |

All compile/check functions funnel through `ModuleCache::resolve` / `ModuleCache::resolve_source`, which is the single entry point to the full pipeline. The CLI and any programmatic callers share exactly the same compilation behavior.

**Warning collection pattern**: Warnings (e.g. empty `@include`) are passed as a `&mut Vec<String>` through the full pipeline — `process_module` → `evaluate` → `evaluate_nodes` → `evaluate_include`. Nothing in the evaluator or resolver calls `eprintln!` directly. The public `compile*` variants print warnings by calling `emit_warnings(&warnings)` on the collected `Vec`. The `compile_collecting_warnings` variants return warnings without printing — this is what the CLI build command uses so it can gate output on the `--quiet` flag.

The CLI `build` and `check` commands both accept `-` as the input path to read from stdin, resolved against the current working directory for import paths.

External dependencies are minimal: `clap` for CLI parsing, `serde_yml`/`serde_json` for frontmatter and runtime vars, `miette`/`thiserror` for rich diagnostic errors, `tempfile` in tests.

## Component Architecture

### Token Model (`src/lexer.rs`)

The lexer converts raw source text into a flat `Vec<Token>`. Token variants cover the complete surface syntax:

- `Text(String, usize)` — raw passthrough text with byte offset
- `Interpolation(String, usize)` — inner content of `{...}` without braces
- `EscapedBrace(usize)` — `\{` → literal `{` at evaluation time
- `Directive(String, usize)` — full line starting with `@`
- `FrontmatterFence(usize)` / `FrontmatterContent(String, usize)` — YAML block
- `CodeFence(String, usize)` / `CodeContent(String, usize)` — fenced code blocks

Code blocks are tokenized as opaque `CodeContent` — no interpolation or directive parsing occurs inside triple-backtick regions. This is enforced at the lexer level; the rest of the pipeline never needs to check for this case.

### AST (`src/ast.rs`)

The `Module` struct holds an optional `Frontmatter` and a `Vec<Node>`. `Node` is an enum with variants for every construct: `Text(TextNode)`, `Interpolation`, `EscapedBrace`, `If`, `For`, `Define`, `Import`, `Export`, `Include`.

`TextNode` is a struct (`{ text: String }`) with no offset field — offsets are not tracked for raw text. Expressions inside `{...}` are further typed as `Expr::Var`, `Expr::Call`, or `Expr::QualifiedCall`.

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
- `functions: HashMap<String, FunctionDef>` — `@define` functions
- `namespaces: HashMap<String, NamespaceScope>` — aliased imports (`@import "x" as ns`)

Lookup always walks from innermost to outermost frame, enabling block-scoped shadowing. `push()`/`pop()` are called around `@for` iterations and function calls.

`FunctionDef` carries three closure capture fields populated at definition time:
- `captured_namespaces: HashMap<String, NamespaceScope>` — alias imports visible at definition site
- `captured_functions: HashMap<String, FunctionDef>` — sibling functions at definition site
- `captured_vars: HashMap<String, Value>` — frontmatter and other vars at definition site

These fields implement lexical scope: a function defined in module A that uses `{ns.fn()}` or `{sibling_fn()}` will work correctly when called from module B, which may not have those names in its scope.

Helper methods `get_all_namespaces()`, `get_all_functions()`, `get_all_vars()` snapshot the current scope for closure capture at definition time.

### Value System (`src/value.rs`)

The `Value` enum has five variants: `String`, `Number(f64)`, `Boolean`, `Array(Vec<Value>)`, `Null`. Objects/maps are explicitly unsupported in v0.1. Truthiness rules match JavaScript-like semantics: `0`, `""`, `[]`, `null`, `false`, and `NaN` are falsy; everything else is truthy.

`Value::Display` renders numbers as integers when the fractional part is zero, guarding against i64 overflow for very large floats. Arrays display as comma-separated values. `Null` displays as empty string.

Both `from_yaml` and `from_json` converters exist, with identical rejection of object/map types. `from_yaml` also handles `serde_yml::Value::Tagged` by unwrapping the tag and recursing.

### Parser (`src/parser.rs`)

The parser converts a token stream to a `Module` AST. Key hardening:

- `MAX_NESTING_DEPTH = 256` — enforced via a `depth` counter on the parser struct; shared between two independent limits: (1) `@if`/`@for`/`@define` block nesting via `enter_block()`, and (2) nested function call argument depth via `parse_args_inner`
- `enter_block()` — extracted helper that increments `self.depth` and returns `Err` if the limit is exceeded; called at the start of `parse_if_block`, `parse_for_block`, and `parse_define_block`, with matching `self.depth -= 1` on exit
- `is_valid_identifier(s)` — all directive names (function names, loop vars, aliases, export names) are validated: must start with ASCII letter or `_`, contain only ASCII alphanumeric or `_`
- Duplicate `@define` parameter names are rejected at parse time
- `@else` without colon gives a targeted error message ("use '@else:' with trailing colon")

**Argument parsing internals**: `parse_args` and `parse_single_arg` are thin public wrappers that delegate to `parse_args_inner(s, depth)` and `parse_single_arg_inner(s, depth)`. The `_inner` variants carry the recursion depth. When a `parse_single_arg_inner` encounters `name(...)` syntax, it produces `Arg::Call` and recurses via `parse_args_inner(inner, depth + 1)`.

`parse_args_inner` tracks open parentheses (`paren_depth`) so that commas inside nested calls are not treated as argument separators at the outer level. Quote-escaped commas inside string arguments are similarly skipped.

### Validator (`src/validator.rs`)

Validates the AST against the current scope **before** evaluation. Catches: undefined variables in `{interpolation}` and `@if` conditions, undefined iterables in `@for`, undefined namespaces in `@include`, undefined functions and arity mismatches in calls, and undefined variable arguments to functions.

**`@for` body validation**: The validator clones the outer scope, injects the loop variable as `Value::Null`, then recurses via top-level `validate()`. Undefined-variable references inside the loop body are caught at validate time.

**`@define` body validation**: The validator clones the outer scope, injects all params as `Value::Null`, then recurses via top-level `validate()`. Both `@for` and `@define` body recursion now delegate to the exported `validate()` function directly (previously an internal `validate_body` helper) — this ensures consistent error reporting.

**`validate_var_args`** is extended to cover all three `Arg` variants:
- `Arg::StringLiteral` — no validation needed
- `Arg::Var` — variable existence check against scope
- `Arg::Call { name, args }` — function existence check, arity check against `func.params.len()`, then recursion into `inner_args` via `validate_var_args`

This means nested calls like `{outer(inner("arg"))}` are fully validated: both `outer` and `inner` must exist with correct arity before evaluation.

The `arity_at` constructor provides source-span-aware arity errors from the validator, in addition to the existing `undefined_var_at`, `undefined_fn_at`, `name_collision_at`, etc.

### Resolver (`src/resolver.rs`)

The resolver is the orchestrator. `ModuleCache` drives the full pipeline for each file and caches `ResolvedModule` by canonical path, preventing repeated work and providing cycle detection.

**Project root detection**: `find_project_root` walks up from the entry file's directory looking for `.git` or `.mdsroot` markers. The found root is stored in `ModuleCache::root_dir` on first resolve. All subsequently resolved paths must `starts_with(root_dir)` — this is the path traversal boundary.

**Security guards** (all checked before reading the file):
1. `validate_import_path` — rejects non-relative paths and null bytes
2. `root_dir` check — rejects paths that escape the project directory
3. `MAX_IMPORT_DEPTH = 64` — rejects chains deeper than 64 levels
4. `MAX_FILE_SIZE = 10MB` — rejects files over 10MB

**Cycle detection** uses two parallel data structures:
- `resolving: HashSet<PathBuf>` — O(1) membership test
- `resolving_stack: Vec<PathBuf>` — insertion-ordered list for cycle path reconstruction (e.g., `a.mds → b.mds → a.mds`)

**`process_module`** is the inner workhorse: it tokenizes, parses, builds scope from frontmatter + runtime vars, walks the AST to register functions (capturing closure scope) and resolve imports, validates, and evaluates. The result is a `ResolvedModule`. Warnings are threaded through as `&mut Vec<String>` from the public API all the way into `evaluate`.

**`ResolvedModule`** fields:
- `functions: HashMap<String, FunctionDef>` — all `@define`d functions (including re-exports)
- `prompt_body: Option<String>` — rendered body text, or None if empty
- `has_explicit_exports: bool` — true once any `@export` appears
- `explicit_exports: HashSet<String>` — the explicitly listed export names

**`prompt` as an export**: Any module with a non-empty body implicitly exports it as `prompt`, unless the module has explicit exports and `"prompt"` is not listed. Importers can bring in the body text:
- `@import { prompt } from "./module.mds"` → binds body text to `prompt` variable
- Merge import of a module with a body → `prompt` variable brought into scope

**Export validation**: After collecting all `@export` directives, the resolver checks every named export either refers to a defined function or is the string `"prompt"`. Exporting an unknown name is a compile error.

**Import semantics**:
- **Alias** (`@import "path" as ns`): resolved module becomes a `NamespaceScope` under `ns`; functions accessed as `{ns.fn(arg)}`
- **Merge** (`@import "path"`): all exported functions brought into scope; frontmatter variables from the imported module are NOT brought in (only functions and `prompt` body)
- **Selective** (`@import { fn } from "path"`): only named exports brought in; `prompt` is handled specially (bound as a variable, not a function)

**Re-export semantics** (`@export name from "path"`, `@export * from "path"`): The source module is resolved and its exports are added to the current module's `functions` map. They are NOT added to the current file's runtime scope — they are only available to modules that import the current module.

**Closure capture**: When a `@define` node is processed, the resolver captures the current scope state into `FunctionDef.captured_*` fields before adding the function to scope. This means a function sees the scope as it existed at its definition point, not at its call point.

### Evaluator (`src/evaluator.rs`)

The evaluator walks the AST and produces the final rendered string. Its public entry point is `evaluate(nodes, scope, warnings)` — the `warnings: &mut Vec<String>` parameter is threaded through all internal helpers including `evaluate_include`. Nothing in the evaluator calls `eprintln!` directly; all diagnostics go into the warnings vec.

The evaluator carries a `call_stack: HashSet<String>` for recursion detection (error on self or mutual recursion) and enforces `MAX_CALL_DEPTH = 128` for deep chains.

`invoke_function` restores the function's captured closure scope (namespaces, functions, vars) before binding parameters, so params shadow captured vars correctly. After evaluation the pushed frame is popped.

**`resolve_args` signature**: `resolve_args(args: &[Arg], scope: &mut Scope, call_stack: &mut HashSet<String>, warnings: &mut Vec<String>) -> Result<Vec<Value>, MdsError>`. The `call_stack` and `warnings` parameters are threaded so `Arg::Call` can invoke `call_function` during argument resolution, which itself may recurse. This is the key wiring that makes nested calls work at evaluation time.

The `Arg::Call` arm in `resolve_args` recursively calls `resolve_args` for inner args, then `call_function` for the nested invocation, wrapping the `String` result in `Value::String`. This means the result of a nested call is always a `String` value regardless of what the inner function produces.

`@include alias` looks up the aliased module's `prompt_body` from the namespace and injects it inline. If the included module has no body text, `evaluate_include` pushes a warning message to `warnings` (not `eprintln!`) and returns an empty string.

`@import` and `@export` nodes are no-ops in the evaluator (handled entirely by the resolver).

## Component Interactions

The data flow is:

```
source text
  → lexer::tokenize()  → Vec<Token>
  → parser::parse()    → Module (AST)
  → resolver: scope built from frontmatter + runtime_vars
  → resolver: imports resolved recursively (ModuleCache)
    → closure scope captured into FunctionDef.captured_*
  → validator::validate()  (uses scope snapshot)
  → evaluator::evaluate(&mut warnings)  → String (raw prompt body)
  → lib::clean_output()    → final Markdown string
```

**Warning propagation**: the `warnings: &mut Vec<String>` vector is allocated in the public API function and passed all the way through `ModuleCache::resolve` → `process_module` → `evaluate` → `evaluate_nodes` → `evaluate_include`. After the pipeline completes, the calling code decides whether to print them (via `emit_warnings`) or return them to the caller (via `compile_collecting_warnings`).

Runtime variables override frontmatter: in `process_module`, frontmatter vars are loaded first, then runtime vars overwrite any key that appears in both. This means `--vars` JSON and `--set KEY=VAL` always win over template defaults.

The `ModuleCache` is created per top-level compile call (not shared across calls). Each entry in `modules` is a `ResolvedModule` clone — the resolver owns the cache and clones out of it on cache hits.

## Integration Patterns

### Adding a New Directive

1. Add a new variant to `Node` in `src/ast.rs` (and any needed sub-structs)
2. Lex: directives are already captured as `Token::Directive` — no lexer change required unless new syntax (e.g., new brace-form)
3. Parse: add a branch in `Parser::parse_directive()` matching the `@name` prefix; validate identifier names with `is_valid_identifier()`
4. Validate: add a match arm in `validate_node()` — validate what the resolver can't catch
5. Resolve: if the directive requires file I/O (import-like), handle it in `process_module`'s AST walk
6. Evaluate: add a match arm in `evaluate_nodes()` — if the directive can emit warnings, accept and forward `warnings: &mut Vec<String>`; `Import`/`Export` stay as no-ops there
7. Add integration test fixture in `tests/fixtures/` and a test in `tests/integration.rs`

### Adding a New Arg Variant

If you add a fourth `Arg` variant, update all three sites that match on `Arg`:
1. `parse_single_arg_inner` in `src/parser.rs` — construct the new variant
2. `resolve_args` in `src/evaluator.rs` — evaluate to a `Value`
3. `validate_var_args` in `src/validator.rs` — pre-evaluation validity check

Failing to update any one of these produces an incomplete `match` compilation error, which is intentional — `Arg` has no wildcard arm.

### Warning-Emitting Code

Any code that needs to emit a non-fatal diagnostic must accept `warnings: &mut Vec<String>` and push to it. Never call `eprintln!` from evaluator, resolver, or library code. The CLI controls whether to print warnings based on the `--quiet` flag.

The two-tier API pattern in `lib.rs`:
- `compile(path, vars)` — internal convenience that calls `compile_collecting_warnings` then `emit_warnings`
- `compile_collecting_warnings(path, vars)` — returns `(String, Vec<String>)` — use this when the caller needs to gate warning output (e.g., the CLI's quiet mode)

### Error Reporting Pattern

All errors are `MdsError` variants (thiserror + miette). For errors with source location, use the `_at` constructor variants:

```rust
// Use _at variants to attach a miette SourceSpan — provides file + line in error output.
// `file` = canonical path string, `source` = full source text, `offset`/`len` = byte range.
MdsError::syntax_at(message, file, source, offset, len)
MdsError::undefined_var_at(name, file, source, offset, len)
MdsError::undefined_fn_at(name, file, source, offset, len)
MdsError::name_collision_at(name, file, source, offset, len)
MdsError::file_not_found_at(path, file, source, offset, len)
MdsError::arity_at(name, expected, got, file, source, offset, len)

// Use bare variants only when source context is unavailable.
MdsError::syntax(message)
MdsError::undefined_var(name)
MdsError::undefined_fn(name)
MdsError::name_collision(name)
MdsError::file_not_found(path)
MdsError::arity(name, expected, got)
MdsError::type_error(got)
MdsError::recursion(name)
MdsError::import_error(message)
MdsError::export_error(message)
```

Always prefer `_at` variants inside the validator and evaluator where source offsets are available from the AST nodes.

### Adding a New Value Type

Currently blocked by design: `Value::from_yaml` and `Value::from_json` both return `Err` for object/map types. Any new value variant must be added to both converters, to `Value::Display`, `Value::is_truthy`, `Value::type_name`, and `Value::as_array` (if relevant). Tests for display edge cases (especially large numbers) exist in `src/value.rs`.

## Constraints

- **Import paths must be relative** — `validate_import_path` rejects non-relative paths (must start with `./` or `../`) and null bytes. Runs before any filesystem access.
- **Path traversal prevention** — resolved import paths must remain within the project root (detected via `.git`/`.mdsroot` walk-up from entry file directory).
- **MAX_IMPORT_DEPTH = 64** — prevents stack overflow from deep chains (separate from circular import detection).
- **MAX_FILE_SIZE = 10MB** — checked before reading; prevents memory exhaustion from large inputs.
- **MAX_CALL_DEPTH = 128** — prevents stack overflow from deeply nested function calls.
- **MAX_NESTING_DEPTH = 256** — shared constant used for two distinct limits: parser-level block nesting (`@if`/`@for`/`@define`) via `enter_block()`, and argument-level nested call depth via `parse_args_inner`.
- **Object types unsupported** — YAML mappings and JSON objects are rejected at the value conversion layer.
- **`.md` files require `type: mds`** in frontmatter to be compiled — `validate_file_type` enforces this.
- **Recursion is detected at evaluation time** using the call stack set — the validator cannot catch recursive call chains because they depend on runtime scope.
- **Nested call result is always a String** — `Arg::Call` evaluation wraps the inner function's output in `Value::String`. Functions that return non-string values (e.g., future numeric functions) will still produce a string when used as a nested argument.

## Anti-Patterns

- **Calling `eprintln!` from evaluator or resolver code** — all non-fatal diagnostics must go through the `warnings: &mut Vec<String>` parameter. Direct stderr output bypasses the quiet flag and makes the warnings un-testable.
- **Calling `evaluate` before `validate`** — the evaluator trusts that all references exist; skipping validation will produce misleading errors at evaluation rather than rich span-aware diagnostics.
- **Resolving imports in the evaluator** — imports must be resolved before evaluation so scope is complete when `validate` runs. Adding import-like behavior in the evaluator breaks this order.
- **Creating `ModuleCache` per-module instead of per-compile** — the cache is the only thing preventing re-parsing the same file dozens of times. Each `compile()` / `compile_str_with()` call creates exactly one cache.
- **Using bare `MdsError::syntax(msg)` when source context is available** — always prefer `syntax_at` when you have an offset and source string.
- **Adding object/map support without updating all Value methods** — `from_yaml`, `from_json`, `Display`, `is_truthy`, `type_name`, and `as_array` must all be consistent.
- **Forgetting to capture closure scope in new definition-like directives** — any directive that defines a callable entity should call `scope.get_all_namespaces()`, `get_all_functions()`, and `get_all_vars()` at definition time so the callable works correctly when invoked from other modules.
- **Adding functions to `process_module`'s scope without also capturing current scope into the FunctionDef** — if you add a function to scope after other functions are already captured, the previously captured siblings won't see the new function.
- **Adding a new `Arg` variant without updating all three match sites** — parser (`parse_single_arg_inner`), evaluator (`resolve_args`), and validator (`validate_var_args`) all pattern-match exhaustively on `Arg`. Adding a variant without updating all three will produce a compile error, which is by design.
- **Passing `resolve_args` without `call_stack` or `warnings`** — nested `Arg::Call` evaluation requires the call stack for recursion detection and warnings for diagnostics. Any future refactor that removes these from `resolve_args` will silently allow unbounded recursion through argument nesting or lose warning context.
- **Using `compile` instead of `compile_collecting_warnings` in CLI code** — the CLI must use the collecting variants to properly gate warning output on the `--quiet` flag.

## Gotchas

- **`@define` body nodes have leading/trailing newlines stripped** — the parser calls `strip_leading_newline` and `strip_trailing_newline` on `@define` bodies. If you add a new block directive, apply the same stripping unless you want those newlines in output.
- **`@for` and `@define` body validation uses a Null-injected clone** — the validator validates both `@for` and `@define` bodies by cloning the outer scope and injecting variables (loop var or params) as `Value::Null`. Type errors that depend on runtime type surface at evaluation time, not validate time.
- **Runtime vars override frontmatter silently** — there is no warning when a runtime var shadows a frontmatter key. Intentional but can cause confusion when debugging.
- **`@export` changes all-implicit to explicit** — once any `@export` appears in a module, only explicitly listed names are exported. Adding an `@export name` to a previously-implicit-all module will break importers depending on other functions.
- **`@export prompt` is valid** — the string `"prompt"` is a special case in export validation. It does not need a corresponding `@define prompt` — it refers to the module's rendered body.
- **`@include` on an empty module pushes a warning and returns empty** — `evaluate_include` calls `warnings.push(...)` (not `eprintln!`) and returns `""`. No error. The include disappears from output. The warning only reaches stderr if the caller chooses to print it.
- **Merged imports bring in `prompt` body but not frontmatter vars** — `@import "path"` (merge) brings functions and the `prompt` body text into scope, but NOT the imported module's frontmatter variables. Two merge-imported modules that both define the same frontmatter variable do not cause a collision.
- **Selective import of `prompt` binds as a variable, not a function** — `@import { prompt } from "path"` sets `prompt` as a `Value::String` via `scope.set_var`, not `scope.set_function`.
- **`compile_str` takes no arguments** — the zero-argument form `compile_str(source)` is a convenience wrapper. Use `compile_str_with(source, base_dir, runtime_vars)` when you need import resolution relative to a specific directory or runtime variable overrides.
- **Closure capture is eager and shallow** — `get_all_vars()` / `get_all_functions()` / `get_all_namespaces()` snapshot the scope at definition time. Functions defined after the closure capture are not visible to the captured function. Within `process_module`, `@define` nodes are processed in top-to-bottom AST order, so later-defined functions are not visible to earlier ones.
- **`compile_str` / `resolve_source` uses a virtual path `<source>`** — in-memory sources cannot be canonicalized. Repeated calls to `compile_str` re-parse every time; there is no caching for in-memory sources.
- **Project root is set on first resolve** — `root_dir` is set lazily. If `resolve_source` is called first, `root_dir` is the provided `base_dir`, not a git-root-discovered path.
- **Cycle detection reconstructs the path from `resolving_stack`** — when circular import is detected, the error builds the chain by finding the repeated path in the stack and joining from that point with `→`. The stack is insertion-ordered, reflecting the actual import sequence.
- **`TextNode` has no offset** — raw text nodes (`Node::Text(TextNode)`) do not carry a byte offset. Only structured nodes (`Interpolation`, `IfBlock`, `ForBlock`, `IncludeDirective`) have offsets for error reporting.
- **`enter_block()` must be paired with `self.depth -= 1`** — the helper only increments; callers are responsible for decrementing after the block body is parsed. Failing to decrement will cause subsequent blocks to see an inflated depth and may reject valid inputs.
- **`parse_single_arg` (no `_inner` suffix) exists only in `#[cfg(test)]`** — the public-facing name is the test shim. In production code, call `parse_single_arg_inner(s, 0)` directly (or use `parse_args`).

## Key Files

- `src/lib.rs` — public API: `compile`, `compile_str`, `compile_str_with`, `compile_collecting_warnings`, `compile_str_collecting_warnings`, `check`, `check_str`, `check_str_with`, `load_vars_file`, `clean_output`
- `src/ast.rs` — all AST types including `Arg::Call` for nested function call arguments; the contract between parser and everything downstream
- `src/lexer.rs` — tokenizer; handles frontmatter, code fences, interpolation, directives
- `src/parser.rs` — converts token stream to `Module` AST; `enter_block()` helper, `parse_args_inner`/`parse_single_arg_inner` depth-bounded recursion, identifier validation, duplicate param detection
- `src/resolver.rs` — orchestrator: drives the full pipeline, module cache, import resolution, closure capture, security guards; threads `&mut Vec<String>` warnings through to evaluate
- `src/evaluator.rs` — AST walker that produces the rendered string; `resolve_args` handles `Arg::Call` via recursive evaluation with shared `call_stack` and `warnings`; `evaluate_include` pushes to warnings vec, never calls `eprintln!`
- `src/validator.rs` — pre-evaluation semantic checks; `validate_var_args` recursively validates nested `Arg::Call` arguments
- `src/scope.rs` — scope chain with frames for variables, functions, and namespaces; closure capture helpers
- `src/value.rs` — runtime value enum with YAML/JSON converters and display rules
- `src/error.rs` — `MdsError` enum with miette diagnostics; builder methods for span-aware errors
- `tests/integration.rs` — end-to-end tests covering all features, error paths, CLI integration (stdin, file output, vars file, quiet flag), edge cases (nested loops, loop shadowing, falsy values, mutual recursion, escape sequences in blocks/functions), and error format verification

## Related

- `src/resolver.rs` — canonical reference for the module system, import semantics, and security guards
- `src/evaluator.rs` — canonical reference for directive execution order, closure restore, call-depth guards, nested arg evaluation, and warning collection
- `src/scope.rs` — canonical reference for closure capture API (`get_all_*` methods)
- `src/ast.rs` — canonical reference for `Arg` variants; any new argument form starts here
- `src/lib.rs` — canonical reference for the two-tier warning API (`compile` vs `compile_collecting_warnings`)
- `tests/integration.rs` — covers all directive combinations including nested function calls, CLI stdin/quiet mode, and edge cases; read before adding new directives to understand existing fixture patterns
