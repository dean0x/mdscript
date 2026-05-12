---
feature: mds-compiler
name: MDS Compiler
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, or debugging output rendering. Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, export, include, define, for, if."
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
updated: 2026-05-12
---

# MDS Compiler

## Overview

MDS (Markdown Script) is a Rust compiler that transforms `.mds` files — Markdown with `@directives` and `{var}` interpolation — into plain Markdown. The primary use case is composable LLM prompt templates: authors write templates with variables, conditionals, loops, and reusable function fragments, then compile them to a final prompt string.

The compilation pipeline is strictly sequential: **lexer → parser → validator → resolver → evaluator → render**. Each layer has a single responsibility and communicates through typed interfaces rather than shared mutable state. The `resolver` is the orchestrator — it drives all other stages and manages the module cache used for imports.

## System Context

The binary is a CLI tool (`mds build`, `mds check`, `mds init`) backed by a library crate. The library exposes three public functions: `compile(path)`, `compile_str(source, base_dir)`, and `check(path)`. All three funnel through `ModuleCache::resolve` / `ModuleCache::resolve_source`, which is the single entry point to the full pipeline. This means the CLI and any programmatic callers share exactly the same compilation behavior.

External dependencies are minimal: `clap` for CLI parsing, `serde_yml`/`serde_json` for frontmatter and runtime vars, `miette`/`thiserror` for rich diagnostic errors.

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

The `Module` struct holds an optional `Frontmatter` and a `Vec<Node>`. `Node` is an enum with variants for every construct: `Text`, `Interpolation`, `EscapedBrace`, `If`, `For`, `Define`, `Import`, `Export`, `Include`. Expressions inside `{...}` are further typed as `Expr::Var`, `Expr::Call`, or `Expr::QualifiedCall`.

All AST nodes carry a byte `offset` into the original source. This is threaded through to `MdsError` variants to produce precise source-span diagnostics via `miette`.

### Scope (`src/scope.rs`)

`Scope` is a stack of `Frame` structs (innermost last). Each frame holds:
- `vars: HashMap<String, Value>` — variable bindings
- `functions: HashMap<String, FunctionDef>` — `@define` functions
- `namespaces: HashMap<String, NamespaceScope>` — aliased imports (`@import "x" as ns`)

Lookup always walks from innermost to outermost frame, enabling block-scoped shadowing (e.g., `@for` loop variables shadow outer variables with the same name). `push()`/`pop()` are called around `@for` iterations and function calls.

### Value System (`src/value.rs`)

The `Value` enum has five variants: `String`, `Number(f64)`, `Boolean`, `Array(Vec<Value>)`, `Null`. Objects/maps are explicitly unsupported in v0.1. Truthiness rules match JavaScript-like semantics: `0`, `""`, `[]`, `null`, and `false` are falsy; everything else is truthy. `Value::Display` renders numbers as integers when the fractional part is zero, guarding against i64 overflow for very large floats.

Both `from_yaml` and `from_json` converters exist, with identical rejection of object/map types. `from_yaml` also handles `serde_yml::Value::Tagged` by unwrapping the tag and recursing into `from_yaml`.

### Validator (`src/validator.rs`)

Validates the AST against the current scope **before** evaluation. Catches: undefined variables in `{interpolation}` and `@if` conditions, undefined iterables in `@for`, undefined namespaces in `@include`, undefined functions and arity mismatches in calls, and undefined variable arguments to functions.

**`@for` body validation**: The validator does validate `@for` loop bodies. It clones the outer scope into an `inner` scope, injects the loop variable as `Value::Null`, then recurses into the body with that inner scope. This means undefined-variable references inside the loop body are caught at validate time, not evaluation time. The iterable variable must exist in the outer scope; the loop var itself does not need to pre-exist.

### Resolver (`src/resolver.rs`)

The resolver is the orchestrator. `ModuleCache` drives the full pipeline for each file and caches `ResolvedModule` by canonical path, preventing repeated work and providing cycle detection.

Cycle detection uses two parallel data structures:
- `resolving: HashSet<PathBuf>` — O(1) membership test for "is this module currently resolving?"
- `resolving_stack: Vec<PathBuf>` — insertion-ordered list of in-progress modules, used to reconstruct the cycle path string (e.g., `a.mds → b.mds → a.mds`)

`process_module` is the inner workhorse: it tokenizes, parses, builds scope from frontmatter + runtime vars, walks the AST to register functions and resolve imports, validates, and evaluates. The result is a `ResolvedModule` with `functions`, `vars`, `prompt_body`, and export metadata.

Import semantics:
- **Alias** (`@import "path" as ns`): resolved module becomes a `NamespaceScope` under `ns`; functions accessed as `{ns.fn(arg)}`
- **Merge** (`@import "path"`): all exports brought into the current scope directly; name collision is an error
- **Selective** (`@import { fn } from "path"`): only named exports brought in; error if name not exported

Export semantics: without any `@export` directives, all `@define` functions are implicitly exported. Once any `@export` appears in a module, only explicitly listed names are exported.

### Evaluator (`src/evaluator.rs`)

The evaluator walks the AST and produces the final rendered string. It carries a `call_stack: HashSet<String>` for recursion detection (error on self or mutual recursion) and enforces `MAX_CALL_DEPTH = 128` for deep chains. Function calls create a new scope frame, bind parameters, evaluate the body, then restore the frame.

`@include alias` looks up the aliased module's `prompt_body` from the namespace and injects it inline. If the included module has no body text, `evaluate_include` silently returns an empty string — no error, no warning.

`@import` and `@export` nodes are no-ops in the evaluator (handled entirely by the resolver).

## Component Interactions

The data flow is:

```
source text
  → lexer::tokenize()  → Vec<Token>
  → parser::parse()    → Module (AST)
  → resolver: scope built from frontmatter + runtime_vars
  → resolver: imports resolved recursively (ModuleCache)
  → validator::validate()  (uses scope snapshot)
  → evaluator::evaluate()  → String (raw prompt body)
  → lib::clean_output()    → final Markdown string
```

Runtime variables override frontmatter: in `process_module`, frontmatter vars are loaded first, then runtime vars overwrite any key that appears in both. This means `--vars` JSON always wins over template defaults.

The `ModuleCache` is created per top-level compile call (not shared across calls). Each entry in `modules` is a `ResolvedModule` clone — the resolver owns the cache and clones out of it on cache hits.

## Integration Patterns

### Adding a New Directive

1. Add a new variant to `Node` in `src/ast.rs` (and any needed sub-structs)
2. Lex: directives are already captured as `Token::Directive` — no lexer change required unless new syntax (e.g., new brace-form)
3. Parse: add a branch in `Parser::parse_directive()` matching the `@name` prefix
4. Validate: add a match arm in `validate_node()` — validate what the resolver can't catch
5. Resolve: if the directive requires file I/O (import-like), handle it in `process_module`'s AST walk
6. Evaluate: add a match arm in `evaluate_nodes()` — `Import`/`Export` stay as no-ops there
7. Add integration test fixture in `tests/fixtures/` and a test in `tests/integration.rs`

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

// Use bare variants only when source context is unavailable (e.g., resolver-level errors
// that occur before or between file reads, where no single source span is meaningful).
MdsError::syntax(message)
MdsError::undefined_var(name)
MdsError::undefined_fn(name)
MdsError::name_collision(name)
MdsError::file_not_found(path)
MdsError::arity(name, expected, got)
MdsError::type_error(got)
MdsError::recursion(name)
```

Bare variants produce no source-span; users see only the error message with no file/line context. Always prefer `_at` variants inside the validator and evaluator where source offsets are available from the AST nodes.

### Adding a New Value Type

Currently blocked by design: `Value::from_yaml` and `Value::from_json` both return `Err` for object/map types. Any new value variant must be added to both converters, to `Value::Display`, `Value::is_truthy`, `Value::type_name`, and `Value::as_array` (if relevant). Tests for display edge cases (especially large numbers) exist in `src/value.rs`.

## Constraints

- **Import paths must be relative** — `validate_import_path` rejects absolute paths and null bytes. This is the primary security boundary for path traversal. The check runs before any filesystem access.
- **MAX_IMPORT_DEPTH = 64** — prevents stack overflow from deep chains (as opposed to circular imports, which are caught separately by `resolving` set).
- **MAX_CALL_DEPTH = 128** — prevents stack overflow from non-recursive but deeply nested function calls.
- **Object types unsupported** — YAML mappings and JSON objects are rejected at the value conversion layer.
- **`.md` files require `type: mds`** in frontmatter to be compiled — `validate_file_type` enforces this.
- **Recursion is detected at evaluation time** using the call stack set — the validator does not (cannot) catch recursive call chains because they depend on runtime scope.

## Anti-Patterns

- **Calling `evaluate` before `validate`** — the evaluator trusts that all references exist; skipping validation will produce panic-free but misleading `UndefinedVariable` errors at evaluation rather than rich diagnostics.
- **Resolving imports in the evaluator** — imports must be resolved before evaluation so scope is complete when `validate` runs. Adding import-like behavior in the evaluator breaks this order.
- **Creating `ModuleCache` per-module instead of per-compile** — the cache is the only thing preventing re-parsing the same file dozens of times in import-heavy projects. Each `compile()` / `compile_str()` call creates exactly one cache.
- **Using bare `MdsError::syntax(msg)` when source context is available** — always prefer `syntax_at` when you have an offset and source string. The bare variants produce no source-span and give users no indication of where the problem is.
- **Adding object/map support without updating all Value methods** — `from_yaml`, `from_json`, `Display`, `is_truthy`, `type_name`, and `as_array` must all be consistent.

## Gotchas

- **`@define` body nodes have leading/trailing newlines stripped** — the parser calls `strip_leading_newline` and `strip_trailing_newline` on `@define` bodies so the function output doesn't include the newlines introduced by the block syntax. If you add a new block directive, apply the same stripping unless you want those newlines in output.
- **`@for` body validation uses a Null-injected clone** — the validator validates `@for` bodies by cloning the outer scope and injecting the loop variable as `Value::Null`. This catches undefined references at validate time, but it means the loop variable appears as type `null` during validation — not its runtime type. Type errors in the body (e.g., using the loop var as an iterable) surface at evaluation time, not validate time.
- **Runtime vars override frontmatter silently** — there is no warning when a runtime var shadows a frontmatter key. This is intentional but can cause confusion when debugging.
- **`@export` changes all-implicit to explicit** — once a module has any `@export` directive, only explicitly exported names are visible to importers. Adding an `@export name` to a module that was previously exporting everything will break other modules that depended on the implicit-all behavior.
- **`@include` on an empty module produces an empty string** — `evaluate_include` silently returns `""` when the aliased module has no `prompt_body`. No error, no warning; the include just disappears from output.
- **Merged imports check for name collisions; aliased imports do not** — `@import "path" as ns` never collides (it's under a namespace), but `@import "path"` (merge) raises `NameCollision` if a function name already exists in scope.
- **`compile_str` uses a virtual path `<source>` in the module cache** — in-memory sources are never cached (the key is a virtual path that can't be canonicalized). Repeated calls to `compile_str` with the same source will re-parse every time.
- **Qualified calls (`{ns.fn()}`) look up function definitions in the namespace at evaluation time** — if the namespace's scope diverges from what validation saw (shouldn't happen normally), evaluation can fail with a different error than validation predicted.
- **Cycle detection reconstructs the path from `resolving_stack`** — when a circular import is detected, the error message is built by finding the repeated path in `resolving_stack` and joining the tail with `→`. The stack is in insertion order, so the cycle chain reflects the actual import sequence.

## Key Files

- `src/lib.rs` — public API: `compile`, `compile_str`, `check`, `load_vars_file`, `clean_output`
- `src/ast.rs` — all AST types; the contract between parser and everything downstream
- `src/lexer.rs` — tokenizer; handles frontmatter, code fences, interpolation, directives
- `src/parser.rs` — converts token stream to `Module` AST; handles all directive syntax
- `src/resolver.rs` — orchestrator: drives the full pipeline, module cache, import resolution
- `src/evaluator.rs` — AST walker that produces the rendered string; manages call stack and scope frames
- `src/validator.rs` — pre-evaluation semantic checks with source-span error reporting
- `src/scope.rs` — scope chain with frames for variables, functions, and namespaces
- `src/value.rs` — runtime value enum with YAML/JSON converters and display rules
- `src/error.rs` — `MdsError` enum with miette diagnostics; builder methods for span-aware errors
- `tests/integration.rs` — end-to-end tests covering all features and error paths

## Related

- `src/resolver.rs` — canonical reference for the module system and import semantics
- `src/evaluator.rs` — canonical reference for directive execution order and call-depth guards
- `tests/integration.rs` — covers all directive combinations; read before adding new directives to understand existing fixture patterns
