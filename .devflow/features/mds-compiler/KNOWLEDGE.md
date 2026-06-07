---
feature: mds-compiler
name: MDS Compiler
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, modifying CLI output behavior, using the virtual filesystem / dependency tracking API, working with @message blocks, messages output mode, or the compile_messages API family. Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, export, include, define, for, if, elseif, negation, equality, Condition, CondValue, And, Or, logical operators, Param, default arguments, And, Or, logical operators, ArityMismatch, BuiltinError, call_function, required_param_count, condvalue_to_value, MAX_LOGICAL_OPERANDS, message, @message, messages mode, compile_messages, compile_messages_str, compile_messages_virtual, CompileMessagesOutput, Message, evaluate_messages, collect_messages, EvalMessage, OutputFormat, --format messages, injection safety, bare-word role, dynamic role, inside_message, total_message_bytes, MAX_MESSAGE_COUNT, MAX_MESSAGES_TOTAL_SIZE, MAX_ARRAY_ELEMENTS, scan_imports, load_vars_file, load_vars_str, check_virtual, compile_file, read_build_input."
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
  - crates/mds-core/tests/api_surface.rs
  - crates/mds-core/tests/messages.rs
  - crates/mds-cli/tests/format_messages.rs
created: 2026-05-12
updated: 2026-06-07
---

# MDS Compiler

## Overview

MDS (Markdown Script) is a Rust compiler that transforms `.mds` files — Markdown with `@directives` and `{var}` interpolation — into plain Markdown. The primary use case is composable LLM prompt templates: authors write templates with variables, conditionals, loops, and reusable function fragments, then compile them to a final prompt string.

The compilation pipeline is strictly sequential: **lexer → parser → validator → resolver → evaluator → render**. Each layer has a single responsibility and communicates through typed interfaces rather than shared mutable state. The `resolver` is the orchestrator — it drives all other stages and manages the module cache used for imports.

The compiler supports two output modes: **text mode** (the default, renders to a Markdown string) and **messages mode** (introduced in Issue #56, compiles `@message` blocks into a structured `Vec<Message>` for LLM chat APIs). The two modes share the same parse/validate pipeline; only the final evaluation step differs.

## System Context

**Cargo workspace**: `mds-core` (library crate, publishes as `mds`) at `crates/mds-core/`; `mds-cli` (binary crate) at `crates/mds-cli/`. The workspace root `Cargo.toml` and `Cargo.lock` are at the repo root.

The library exposes public `compile*` / `check*` / `compile_messages*` functions (all carry `#[must_use]`). Public types include: `FileSystem`, `NativeFs`, `VirtualFs`, `ModuleCache`, `Value`, `MdsError`, `SerializedError`, `SerializedSpan`, `CompileOutput`, `CompileMessagesOutput`, `Message`, and constants `MAX_FILE_SIZE` / `MAX_TRAVERSAL_DEPTH`.

**Utility functions added alongside messages mode**:
- `pub fn compile_file(path: &str) -> Result<String, MdsError>` — thin wrapper over `compile(Path::new(path), None)` for callers that have a string path.
- `pub fn scan_imports(source: &str) -> Result<Vec<String>, MdsError>` — parses the AST and returns all import/re-export paths (frontmatter `imports:` first, then body `@import`/`@export ... from` directives), deduplicated in insertion order. Returns a compile error on syntax error.
- `pub fn load_vars_file(path: &Path) -> Result<HashMap<String, Value>, MdsError>` — reads a JSON file and parses it as a vars map. Enforces `MAX_FILE_SIZE`.
- `pub fn load_vars_str(json: &str) -> Result<HashMap<String, Value>, MdsError>` — parses a JSON string as a vars map. Enforces `MAX_FILE_SIZE` on the input length.
- `pub fn check_virtual(modules, entry, vars) -> Result<(), MdsError>` — validates a virtual-filesystem module, printing warnings to stderr.
- `pub fn check_virtual_collecting_warnings(modules, entry, vars) -> Result<((), Vec<String>), MdsError>` — same but returns warnings without printing.

All compile/check functions funnel through `ModuleCache::resolve` / `ModuleCache::resolve_source`, the single entry point to the full pipeline. The messages-mode counterparts (`resolve_key_messages`, `resolve_source_messages`) follow the same pattern but call `evaluate_messages` instead of `evaluate`.

**Warning collection pattern**: Warnings pass as `&mut Vec<String>` through the full pipeline. Nothing in the evaluator or resolver calls `eprintln!` directly.

The library module tree includes `pub(crate) mod builtins` (declared in `lib.rs`) which holds the 18 built-in functions.

## Component Architecture

### Limits Module (`crates/mds-core/src/limits.rs`)

All cross-pipeline defense-in-depth constants. Current set:

- `pub(crate) const MAX_DOT_SEGMENTS: usize = 32`
- `pub(crate) const MAX_NESTING_DEPTH: usize = 64`
- `pub(crate) const MAX_ELSEIF_BRANCHES: usize = 256`
- `pub(crate) const MAX_LOGICAL_OPERANDS: usize = 16` — caps leaf operands in a single `&&`/`||` expression
- `pub(crate) const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024`
- `pub(crate) const MAX_TRAVERSAL_DEPTH: usize = 256`
- `pub(crate) const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024` — 50 MB cap on text-mode output
- `pub(crate) const MAX_ARRAY_ELEMENTS: usize = 100_000` — caps `split()` output to prevent runaway array allocation from adversarial inputs
- `pub(crate) const MAX_FRONTMATTER_IMPORTS: usize = 256` — caps `imports` entries in YAML frontmatter
- `pub(crate) const MAX_MESSAGE_COUNT: usize = 10_000` — messages mode: maximum messages producible in one compilation
- `pub(crate) const MAX_MESSAGES_TOTAL_SIZE: usize = MAX_OUTPUT_SIZE` — messages mode: cumulative byte cap across all message content (= 50 MB)

`MAX_FILE_SIZE` and `MAX_TRAVERSAL_DEPTH` are also re-exported as `pub const` from `lib.rs` for use by `mds-cli` and the napi/wasm layers.

`MAX_ARRAY_ELEMENTS` is used in `builtins.rs` to cap `split()` output. It is NOT exported from `lib.rs` — it is an internal limit.

When adding a limit used by more than one pipeline stage, add it to `limits.rs`.

### Evaluator Constants (`crates/mds-core/src/evaluator.rs`)

Three module-level constants defined directly in `evaluator.rs` (not in `limits.rs`):

- `const MAX_CALL_DEPTH: usize = 128` — maximum call stack depth for user-defined function invocations
- `const MAX_TOTAL_ITERATIONS: usize = 1_000_000` — cumulative iteration cap across all `@for` loops in one compilation
- `const MAX_WARNINGS: usize = 1_000` — maximum warnings added to `ctx.warnings` before suppression

These are evaluator-specific and not shared with other pipeline stages.

### Built-in Functions (`crates/mds-core/src/builtins.rs`)

18 built-in functions organized into three groups. User-defined functions shadow built-ins with the same name (shadowing is checked first in `call_function`).

**String:** `upper`, `lower`, `trim`, `replace(str, from, to)`, `starts_with(str, prefix)`, `ends_with(str, suffix)`, `contains(str_or_array, needle)`, `slice(str_or_array, start[, end])`, `string(val)`

**Array:** `split(str, sep)`, `join(array, sep)`, `length(str_or_array)`, `first(array)`, `last(array)`, `reverse(str_or_array)`, `sort(array)`, `unique(array)`

**Type conversion:** `string(val)`, `number(val)`

Two `pub(crate)` functions are the entire interface:
- `get_builtin(name: &str) -> Option<&'static BuiltinMeta>` — used by validator and evaluator for existence checks and arity bounds
- `call_builtin(name: &str, args: &[Value]) -> Result<Value, MdsError>` — dispatches to the private per-function implementations

`BuiltinMeta` carries `name`, `min_args`, `max_args`, and `handler: fn(&[Value]) -> Result<Value, MdsError>`. The `BUILTINS` static array is the single source of truth.

`split()` enforces `MAX_ARRAY_ELEMENTS = 100_000` — if the separator would produce more than that many elements, a `ResourceLimit` error is returned.

### AST (`crates/mds-core/src/ast.rs`)

**`Node::Message(MessageBlock)`** — top-level node variant for `@message` blocks.

**`MessageBlock` struct**:
```rust
pub struct MessageBlock {
    pub role: Expr,   // bare word → StringLiteral; {expr} → parse_expr_inner result
    pub body: Vec<Node>,
    pub offset: usize,
}
```

The role field stores the parsed role expression. Bare words like `system` become `Expr::StringLiteral("system")`. Brace forms like `{role_var}` go through `parse_expr_inner` and become any valid `Expr` variant. This follows ADR-010 — the same grammar shared by interpolation and all directive parsing.

**`Condition` enum** — six variants. Leaves hold `Expr` instead of `Vec<String>` / `CondValue`:

| Variant | Syntax | Notes |
|---|---|---|
| `Condition::Truthy(Expr)` | `@if flag:` or `@if func(x):` | truthy check on any expr |
| `Condition::Not(Expr)` | `@if !flag:` or `@if !func(x):` | negated truthy |
| `Condition::Eq(Expr, Expr)` | `@if func(a) == func(b):` | both sides are expressions |
| `Condition::NotEq(Expr, Expr)` | `@if role != "admin":` | both sides are expressions |
| `Condition::And(Vec<Condition>)` | `@if a && b:` | short-circuit AND |
| `Condition::Or(Vec<Condition>)` | `@if a \|\| b:` | short-circuit OR |

**`Condition` does not derive `PartialEq`** because `Expr::NumberLiteral(f64)` uses IEEE 754 semantics where `NaN != NaN`. **`condition.root()` and `condition.path()` were removed in PR #76** — match on the variant directly.

**`Expr` enum** — the unified expression type shared between interpolation `{ }`, directive conditions/iterables, and `@message` role expressions:

| Variant | Example |
|---|---|
| `Expr::Var(String)` | `{name}`, `@if flag:` |
| `Expr::Call { name, args }` | `{greet("Alice")}`, `@if func(x):` |
| `Expr::QualifiedCall { namespace, name, args }` | `{utils.greet("Alice")}` |
| `Expr::MemberAccess { object, fields }` | `{config.key}`, `@if config.debug:` |
| `Expr::StringLiteral(String)` | bare-word `@message system:` role, `@if x == "admin":` |
| `Expr::NumberLiteral(f64)` | `@if count == 42:` |
| `Expr::BooleanLiteral(bool)` | `@if x == true:` |
| `Expr::NullLiteral` | `@if x == null:` |

**`ForBlock.iterable: Expr`** — any expression accepted by `parse_expr_inner` is valid as a `@for` iterable.

**`Arg` enum** — seven variants: `StringLiteral`, `NumberLiteral`, `BooleanLiteral`, `NullLiteral`, `Var`, `Call { name, args }`, `MemberAccess { object, fields }`.

**`Param` struct** — `name: String`, `default: Option<CondValue>`. Note: `CondValue` and `Expr` literal variants are structurally identical (tracked as tech debt #78 — unification deferred).

**`required_param_count(params: &[Param]) -> usize`** — lives in `ast.rs`. Both validator and evaluator import it from `crate::ast`. Do not look for it in `evaluator.rs`.

### Scope (`crates/mds-core/src/scope.rs`)

**`FunctionDef.params: Vec<Param>`** — unchanged. The `CapturedScope` struct, `Arc<FunctionDef>` in frames, and all `get_all_*` methods are unchanged.

### Parser (`crates/mds-core/src/parser.rs`, `parser_helpers.rs`)

**`parse_expr_inner(s: &str) -> Result<Expr, MdsError>`** (in `parser_helpers.rs`) — the unified expression parser. Used by `parse_interpolation_expr` (for `{...}`), directive parsers (`parse_simple_condition`, `parse_for_directive`), and `parse_message_block` (for `{role_expr}` forms). Handles: variable paths, dot-paths/member access, function calls, qualified calls, and literal values (string, number, boolean, null). This is the key shared grammar point per ADR-010.

**`parse_message_block(rest: &str, offset: usize) -> Result<Node, MdsError>`** — parses a `@message role:` ... `@end` block. Role parsing rules:
1. If the role string starts with `{` and ends with `}`, the inner content is parsed via `parse_expr_inner` (dynamic role).
2. Otherwise, the bare word is wrapped as `Expr::StringLiteral` (static role).
3. An empty role string is a hard parse error.
4. Nested `@message` blocks are rejected via the `inside_message: bool` flag on the `Parser` struct.

**`strip_trailing_directive_colon(s: &str) -> Option<&str>`** (in `parser_helpers.rs`) — strips the trailing `:` from a directive line. Quote-and-paren-aware. Returns `None` if no valid trailing colon. Used by `parse_message_block` as well as `@if`/`@for`/`@define`.

**`find_unquoted_operator`** and **`split_on_unquoted_op`** — both have paren-depth tracking so operators inside `func(a || b)` are not treated as condition-level operators.

**Condition precedence parser** (`parse_condition(s)` in `parser_helpers.rs`):
1. Splits on `||` first (lower precedence) → `Condition::Or` if multiple segments
2. Each segment through `parse_and_level` → splits on `&&` → `Condition::And`
3. Leaves through `parse_simple_condition` (truthy/not/eq/neq)

`count_leaf_operands(condition)` recursively counts leaf operands. Exceeding `MAX_LOGICAL_OPERANDS = 16` → syntax error.

**Default parameter parsing**: `parse_define_block` parses `name(param1, param2 = "default"):` syntax. Parameters with defaults must come after required parameters.

**Injection safety invariant**: Tokenization recognizes `@message` / `@end` on the **original source only**. Variable substitution and role expression evaluation happen at eval time, after the full AST is built. Content inside a `@message` body is never re-tokenized — a variable that expands to `@end` cannot escape the block.

### Validator (`crates/mds-core/src/validator.rs`)

**`Node::Message` arm** — validates the role expression (calls `validate_expr` on `block.role`) and recurses into `block.body` with the same scope. An invalid role expression (e.g. an undefined variable) is caught at validation time, before any evaluation.

**`validate_condition`** — handles `And`/`Or` recursively. For leaves: validates `Expr::Var` and `Expr::MemberAccess` roots against scope; validates `Expr::Call` / `Expr::QualifiedCall` against known functions and builtins.

**`validate_expr` for `Expr::Call`** — checks builtins before rejecting as undefined:
1. Try `scope.get_function(name)` (user-defined) — check arity with `required_param_count`/`total`
2. Try `crate::builtins::get_builtin(name)` — check arity with `meta.min_args`/`meta.max_args`
3. Otherwise: `MdsError::undefined_fn_at`

Imports `required_param_count` from `crate::ast` (not from evaluator).

### Evaluator (`crates/mds-core/src/evaluator.rs`)

**`EvalContext` struct** (`pub(crate)`) — fields:
- `call_stack: Vec<String>` — recursion detection stack (O(n) scan, bounded by `MAX_CALL_DEPTH = 128`)
- `total_iterations: usize` — cumulative `@for` iteration counter, bounded by `MAX_TOTAL_ITERATIONS = 1_000_000`
- `total_message_bytes: usize` — cumulative message content bytes in messages mode, bounded by `MAX_MESSAGES_TOTAL_SIZE`
- `warnings: &mut Vec<String>` — borrowed reference to the warnings accumulator; new warnings are capped at `MAX_WARNINGS = 1_000`

**Text-mode `Node::Message` handling** — in text mode, `@message` blocks are transparent: the body is rendered inline and the role marker is ignored. This maintains full backward compatibility — templates with `@message` blocks compile to plain Markdown without the wrapper syntax.

**`evaluate_messages(nodes, scope, warnings) -> Result<Vec<EvalMessage>, MdsError>`** — public entry point for messages-mode evaluation. Creates an `EvalContext` and calls `collect_messages`.

**`collect_messages(nodes, scope, ctx, out)`** — recursive collector. Handles each `Node` variant:
- `Node::Text` — orphan text outside `@message`: emits a warning if non-whitespace (capped at `MAX_WARNINGS`)
- `Node::EscapedBrace` — orphan escaped brace: silently ignored
- `Node::Interpolation` — orphan interpolation outside `@message`: emits a warning (capped at `MAX_WARNINGS`)
- `Node::Message` — calls `collect_single_message`
- `Node::If` — calls `collect_messages_from_if` (recurses into taken branch)
- `Node::For` — calls `collect_messages_from_for`
- `Node::Define` / `Node::Import` / `Node::Export` — already handled by resolver; ignored
- `Node::Include` — emits a warning: `"warning: @include '{alias}' inside messages mode is ignored"` (capped at `MAX_WARNINGS`)

**`collect_single_message(block, scope, ctx, out)`** — evaluates `block.role` via `evaluate_expr`, evaluates `block.body` via `evaluate_nodes` (text-mode evaluation of the body into a string), trims the result, then checks `MAX_MESSAGE_COUNT` and `MAX_MESSAGES_TOTAL_SIZE` before pushing the `EvalMessage`. Empty messages are silently skipped. A role that evaluates to an empty or whitespace-only string is a runtime type error (mirrors the parse-time check).

**`EvalMessage` struct** (`pub`, defined in `evaluator.rs`):
```rust
pub struct EvalMessage {
    pub role: String,
    pub content: String,
}
```
Converted to the public `Message` type in `lib.rs` before returning from `compile_messages*` functions.

**`evaluate_expr(expr: &Expr, scope, ctx) -> Result<Value, MdsError>`** — evaluates any `Expr` to a `Value`. Shared entry point for interpolation and directive evaluation (including `@message` role expressions).

**`values_equal_runtime(lhs: &Value, rhs: &Value) -> bool`** — replaces the old `values_equal(Value, CondValue)`. Used by `Eq`/`NotEq` condition evaluation.

**`condvalue_to_value(cv: &CondValue) -> Value`** (`pub(crate)`) — converts compile-time `CondValue` literals to runtime `Value`. Used in `invoke_function` to supply default argument values.

`required_param_count` is imported from `crate::ast`.

### Resolver (`crates/mds-core/src/resolver.rs`)

**Messages-mode resolution path** — two public methods on `ModuleCache`:
- `resolve_key_messages(key, runtime_vars, warnings) -> Result<Vec<EvalMessage>, MdsError>` — resolves a virtual-filesystem module in messages mode
- `resolve_source_messages(source, base_dir: &str, runtime_vars, warnings) -> Result<Vec<EvalMessage>, MdsError>` — resolves from a source string in messages mode (note: `base_dir` is `&str`, not `&Path`)

Both delegate to `process_module_messages`, which shares the tokenize/parse/build-scope/validate setup with `process_module` but calls `evaluate_messages` at the end.

**`ModuleCache::resolve_source` takes `base_dir: &str`** (not `&Path`) — the internal `resolve_base_dir` helper converts `Option<&Path>` to a UTF-8 `String` at the `lib.rs` level. Public callers go through `lib.rs` wrappers that accept `Option<&Path>`.

**No-`@message`-blocks hard error** — `process_module_messages` checks `has_message_block(&module.body)` after validation. If no `@message` block is found, it returns `MdsError::syntax("compile_messages requires at least one @message block, but none were found in the template")`. This is not a silent fallback — it is a compile error.

**Frontmatter imports** (from PR #85):

`FrontmatterImport` enum with three variants:
- `Alias { path: String, alias: String }` — `imports: [{path: "x.mds", as: alias}]`
- `Merge { path: String }` — `imports: [{path: "x.mds"}]`
- `Selective { path: String, names: Vec<String> }` — `imports: [{path: "x.mds", names: [greet]}]`

Key functions:
- `parse_frontmatter_imports_from_yaml(val: &serde_yaml_ng::Value) -> Result<Vec<FrontmatterImport>, MdsError>` (pub(crate)) — parses the `imports` YAML value
- `parse_frontmatter_imports(raw: &str) -> Result<Vec<FrontmatterImport>, MdsError>` (pub(crate)) — parses raw YAML frontmatter string to extract import list; used by `scan_imports` in `lib.rs`

**Resolution order**: frontmatter imports are resolved BEFORE body `@import` directives. A namespace collision between frontmatter and body is a hard compile error.

**`.md` file handling**: The `imports` key is treated as a regular variable in plain `.md` files. Only `.mds` files and `.md` files with `type: mds` in frontmatter trigger import processing. An empty `names: []` selective import is a compile error.

**Output stripping**: `imports` is stripped from the compiled output (like `type: mds`).

**Limit**: `MAX_FRONTMATTER_IMPORTS = 256` enforced in `parse_frontmatter_imports_from_yaml`.

### Public API: `compile_messages` family (`crates/mds-core/src/lib.rs`)

Three-tier API, mirroring the `compile*` family:

| Function | Input | Returns |
|---|---|---|
| `compile_messages_str(source)` | string | `Result<CompileMessagesOutput>` |
| `compile_messages_str_with_deps(source, base_dir, vars)` | string + options | `Result<CompileMessagesOutput>` |
| `compile_messages_virtual(modules, entry, vars)` | virtual FS | `Result<CompileMessagesOutput>` (warns to stderr) |
| `compile_messages_virtual_with_deps(modules, entry, vars)` | virtual FS | `Result<CompileMessagesOutput>` |

Note: there is no `compile_messages_str_with` function — the middle tier that accepts options but does not return deps was not added for the messages family. `compile_messages_str_with_deps` is the lowest-level string API; it does not print warnings.

**`CompileMessagesOutput`**:
```rust
pub struct CompileMessagesOutput {
    pub messages: Vec<Message>,   // structured chat messages
    pub warnings: Vec<String>,    // orphan-text and other non-fatal diagnostics
    pub dependencies: Vec<String>,// imported module keys, depth-first
}
```

**`Message`**:
```rust
pub struct Message {
    pub role: String,    // evaluated role string (e.g. "system", "user")
    pub content: String, // rendered body text (trimmed)
}
```

Both types derive `serde::Serialize` — `serde_json::to_string_pretty` on the `messages` array is what the CLI `--format messages` path uses.

All `compile_messages*` functions carry `#[must_use]`.

### CLI: `--format messages` (`crates/mds-cli/src/main.rs`)

**`OutputFormat` enum**:
```rust
enum OutputFormat {
    #[default]
    Markdown,
    Messages,
}
```

The `build` subcommand accepts `--format <FORMAT>` (values: `markdown`, `messages`). In messages mode:
- The output-dir / `mds.json` project-config logic is **skipped** — output always goes to stdout (or `-o`).
- The compiler calls `compile_messages_str_with_deps` instead of `compile*` — source is read via `read_build_input`, not the `mds::compile_collecting_warnings` path.
- The result's `messages` array is serialized with `serde_json::to_string_pretty` and written as `{json}\n`.

**`read_build_input(input: &Path) -> Result<(String, PathBuf)>`** — shared helper used by the messages-mode path (and available for other paths). Handles stdin (`-`) and file paths. Enforces `MAX_FILE_SIZE` on file reads (reads at most `MAX_FILE_SIZE + 1` bytes and errors if exceeded). Returns `(source_string, base_dir)`. This ensures the messages-mode path has the same size defense-in-depth as file-path compilation.

**`read_stdin() -> Result<(String, PathBuf)>`** — reads from stdin, also enforcing `MAX_FILE_SIZE + 1` byte limit. Returns `(source, cwd)` where `cwd` is the current working directory used as `base_dir`.

Warnings from `CompileMessagesOutput::warnings` are still emitted to stderr (same as text mode).

### Error System (`crates/mds-core/src/error.rs`)

**`ArityMismatch` variant** — fields: `expected_min: usize`, `expected_max: usize`. Display uses `format_arity(min, max)`. Always pass both min and max to `MdsError::arity` / `MdsError::arity_at`.

**`BuiltinError` variant** — `{ message, span, src }`. Constructor: `MdsError::builtin_error(msg)`.

**`ResourceLimit` variant** — used for file size, output size, message count, and cumulative message bytes. Constructor: `MdsError::resource_limit(msg)`.

**`TypeError` variant** — used when a `@for` loop receives a non-array value, or when a `@message` role evaluates to a non-string or empty string. Constructor: `MdsError::type_error(msg)`.

## Component Interactions

The data flow is unchanged: lexer → parser → resolver → validator → evaluator → lib::build_output. Key cross-component dependencies:

- **`ast.rs`**: defines `required_param_count` and `MessageBlock` — imported by evaluator, validator, resolver, and parser
- **`parser_helpers.rs`**: `parse_expr_inner` is the shared grammar entry point for interpolation (`parser.rs`), directive parsing (`parser_helpers.rs`), and `@message` role parsing (`parser.rs::parse_message_block`)
- **`resolver.rs`**: `parse_frontmatter_imports` (pub(crate)) used by `scan_imports` in `lib.rs`; `process_module_messages` is the messages-mode orchestrator
- **`evaluator.rs`**: `evaluate_messages` / `collect_messages` form the messages-mode evaluation path; `evaluate_nodes` is reused to render `@message` body content
- **`builtins.rs`**: `get_builtin` is called from both `validator.rs` and `evaluator.rs`

## Integration Patterns

### Adding a `@message`-Aware Feature

If a feature needs to work inside `@message` bodies (e.g. a new directive), verify behavior in both modes:
1. **Text mode**: `Node::Message` renders the body inline via `evaluate_nodes`. The new directive's `evaluate_nodes` arm handles it automatically.
2. **Messages mode**: `collect_messages` calls `evaluate_nodes` on the body of each `@message` block. The new directive inside a body works automatically. If the directive can appear *outside* a `@message` block in messages mode, add a branch in `collect_messages` to handle it (see how `@if` / `@for` are handled).
3. **Validator**: Add a `Node::YourDirective` arm in `validate_node` — it must run in both modes since validation is shared.

### Adding a Built-in Function

1. Add a `BuiltinMeta { name, min_args, max_args, handler }` entry to the `BUILTINS` static slice in `builtins.rs`
2. Add a `"name" => builtin_name(args)` arm in `call_builtin`'s match
3. Write the private `fn builtin_name(args: &[Value]) -> Result<Value, MdsError>` using `require_string` / `require_string_at` helpers
4. Validator and evaluator automatically recognize the new function through `get_builtin` — no changes needed there

### Adding a New Directive

1. Add a new variant to `Node` in `ast.rs`
2. Parse: add a branch in `Parser::parse_directive()` matching the `@name` prefix; update the unknown-directive error message to list the new directive name
3. Validate: add a match arm in `validate_node()`
4. Resolve: handle in `collect_definitions_and_imports` (file I/O) or `build_scope_from_frontmatter` (scope-only)
5. Evaluate (text mode): add a match arm in `evaluate_nodes()`
6. Evaluate (messages mode): add handling in `collect_messages()` if the directive can appear outside `@message` blocks

### Adding a New Expression Form

If you need a new `Expr` variant:
1. Add to `Expr` enum in `ast.rs`
2. Add parsing in `parse_expr_inner` in `parser_helpers.rs`
3. Add evaluation in `evaluate_expr` in `evaluator.rs`
4. Add validation in `validate_expr` in `validator.rs`

All four sites have exhaustive matches — missing arms produce compile errors.

### Adding a New Arg Variant

If you add an eighth `Arg` variant, update all three sites:
1. `parse_single_arg_inner` in `parser_helpers.rs` — construct the new variant
2. `resolve_args` in `evaluator.rs` — evaluate to a `Value`
3. `validate_var_args` in `validator.rs` — pre-evaluation validity check

### Adding a Frontmatter-Processed Key

Follow the pattern used by `type: mds` and `imports`:
1. Check for the key in `build_scope_from_frontmatter` in `resolver.rs`
2. Remove it from the scope or handle it before passing remaining keys to the scope builder
3. Return the extracted value alongside the `Scope` in the function return type
4. Strip from output by adding to the exclusion list in `strip_reserved_keys`

### Adding a New Public API Function

When adding a new public function to `lib.rs`:
1. Add `#[must_use]` annotation
2. Add the symbol to `crates/mds-core/tests/api_surface.rs` (public API regression test)
3. For functions that accept user input, enforce resource limits (file size, etc.)
4. Follow the `*_collecting_warnings` naming pattern for functions that return warnings without printing

## Anti-Patterns

- **Calling `eprintln!` from evaluator or resolver code** — use `ctx.warnings` or `warnings: &mut Vec<String>`.
- **Calling `evaluate` before `validate`** — the evaluator trusts all references exist.
- **Creating `ModuleCache` per-module instead of per-compile** — destroys caching.
- **Using bare `MdsError::syntax(msg)` when source context is available** — prefer `syntax_at`.
- **Directly interpolating `Value::Object`** — `{obj}` is a runtime error; use `{obj.key}`.
- **Adding a new `Arg` variant without updating all three match sites** — parser, evaluator, validator all match exhaustively.
- **Adding a new `Condition` variant without updating `validate_condition`** — compound conditions require recursive traversal.
- **Adding a new `Expr` variant without updating all four match sites** — parser, evaluator, validator, and any direct Expr matches in tests.
- **Adding a new `Node` variant without updating `collect_messages`** — if a node can appear outside `@message` blocks in a messages-mode template (e.g. `@for`), `collect_messages` must handle it; a missing arm will silently drop the node.
- **Calling `condition.root()` or `condition.path()`** — removed in PR #76. Match on the variant directly.
- **Looking for `required_param_count` in `evaluator.rs`** — it moved to `ast.rs` in PR #76.
- **Using `values_equal(Value, CondValue)` for condition equality** — replaced by `values_equal_runtime(Value, Value)` in PR #76.
- **Calling `arity` / `arity_at` with a single `expected` value** — both now require `expected_min` and `expected_max`.
- **Placing a required param after a param with a default** — the parser rejects this at parse time.
- **Matching exhaustively on `MdsError` or `Value` in external code** — both are `#[non_exhaustive]`.
- **Processing body `@import` before frontmatter `imports`** — frontmatter imports must resolve first.
- **Treating `imports` as a user variable in `.mds` files** — it is a reserved frontmatter key in `.mds` and `.md` files with `type: mds`. Plain `.md` files without `type: mds` keep `imports` as a regular variable.
- **Using `compile_messages*` on a template without `@message` blocks** — this is a hard compile error, not a silent empty result.
- **Nesting `@message` inside another `@message`** — rejected at parse time via the `inside_message` flag.
- **Expecting `--format messages` to use the output-dir / `mds.json` logic** — messages mode always writes to stdout or `-o`; the project config is bypassed.
- **Using `compile_collecting_warnings` for messages mode in the CLI** — messages mode calls `compile_messages_str_with_deps` via `read_build_input`, not the `compile_collecting_warnings` path. The distinction matters for file size enforcement.
- **Calling `scan_imports` and expecting it to fail silently on bad syntax** — it returns a `Result` and propagates syntax errors.
- **Calling `ModuleCache::resolve_source` with a `&Path` directly** — the method takes `base_dir: &str`. Convert via `path.to_str()` or go through `lib.rs` wrappers.

## Gotchas

- **`Condition` does not derive `PartialEq`** — `Expr::NumberLiteral(f64)` uses IEEE 754 where `NaN != NaN`. Implement `PartialEq` manually if needed.
- **`Condition` leaves now hold `Expr`, not `Vec<String>`** — code written against the pre-PR #76 AST will not compile. The path is through `evaluate_expr`, not a field lookup.
- **`parse_expr_inner` is the unified grammar** — both `{interpolation}` and `@directive` expressions (including `@message` role expressions) go through the same function. A bug in `parse_expr_inner` affects all three contexts.
- **`strip_trailing_directive_colon` is paren-aware** — `@if func(a:b):` strips only the final colon. Earlier naive colon stripping would have broken on such inputs.
- **`required_param_count` is in `ast.rs`, not `evaluator.rs`** — importing from the wrong module is a compile error.
- **`MAX_LOGICAL_OPERANDS = 16` is a leaf count, not a per-level count** — `a && b || c && d` has 4 leaf operands.
- **`And`/`Or` conditions are validated conservatively** — the validator checks all operands even though evaluation short-circuits at runtime.
- **Frontmatter `imports` is stripped from output** — it does not appear in the rendered Markdown.
- **Empty `names: []` in frontmatter selective import is a compile error** — not a no-op.
- **`CondValue` and `Expr` literal types are near-duplicates** — tracked as tech debt issue #78. Do not unify them without a dedicated PR.
- **`call_function` returns `Value`, not `String`** — code that previously expected `call_function` to return `Result<String>` must be updated.
- **Key-value iteration sorts keys alphabetically** — YAML insertion order is not preserved.
- **`call_stack` is `Vec`, not `HashSet`** — recursion detection uses O(n) scan at MAX_CALL_DEPTH=128.
- **Orphan text in messages mode is a warning, not an error** — text outside any `@message` block is silently skipped with a warning appended to `CompileMessagesOutput::warnings`. It is NOT rendered.
- **Orphan `@include` in messages mode emits a warning** — `collect_messages` handles `Node::Include` with a warning rather than silently ignoring it, so callers can detect unexpected use.
- **Orphan interpolation in messages mode emits a warning** — `Node::Interpolation` outside any `@message` block produces a warning (same as orphan text).
- **`@message` body content is evaluated in text mode** — `collect_single_message` calls `evaluate_nodes` (the same function used for text-mode output). The result is trimmed before being stored as `content`.
- **`MAX_MESSAGES_TOTAL_SIZE` is a cumulative cap** — it applies to the sum of all `content` lengths across all messages, not per-message. A template with 10,000 messages each just under the per-message limit could still hit this cap.
- **Injection safety**: `@message`/`@end` tokenization runs on the original source before any variable substitution. A variable containing literal `@end` text cannot break out of a message block body — it is never re-tokenized.
- **`EvalMessage` is not purely internal** — it is `pub` and lives in `evaluator.rs`, but it is converted to the public `mds::Message` type in `lib.rs` before leaving the crate. The `pub` is needed because `resolver.rs` receives and returns `Vec<EvalMessage>` from `process_module_messages`. Do not expose `EvalMessage` through the public API.
- **`MAX_ARRAY_ELEMENTS` is not exported** — it is `pub(crate)` in `limits.rs`. It is not part of the public API and should not be referenced outside of `builtins.rs`.
- **`read_build_input` enforces `MAX_FILE_SIZE`** — this is a defense-in-depth measure for the CLI's messages mode. The napi layer has its own `check_source_size`. Ensure new CLI input paths (stdin or file) go through `read_build_input` or `read_stdin` rather than raw `std::fs::read_to_string`.

## Key Files

- `crates/mds-core/src/limits.rs` — all cross-pipeline resource limits; `MAX_MESSAGE_COUNT = 10_000`, `MAX_MESSAGES_TOTAL_SIZE`, and `MAX_ARRAY_ELEMENTS = 100_000` added for messages mode and split() safety
- `crates/mds-core/src/ast.rs` — all AST types; `Node::Message(MessageBlock)` added; `Condition` variants hold `Expr`; `ForBlock.iterable: Expr`; `Param` struct; `required_param_count` function
- `crates/mds-core/src/builtins.rs` — 18 built-in functions; `BuiltinMeta` struct; `get_builtin` / `call_builtin` entry points; `split()` enforces `MAX_ARRAY_ELEMENTS`
- `crates/mds-core/src/parser_helpers.rs` — `parse_expr_inner` (shared expression grammar); `strip_trailing_directive_colon`; condition precedence parser; default param parsing; `find_unquoted_operator` and `split_on_unquoted_op` with paren-depth tracking
- `crates/mds-core/src/parser.rs` — `parse_message_block`; `inside_message` flag; role parsing (bare-word vs `{expr}`)
- `crates/mds-core/src/evaluator.rs` — `evaluate_expr` (Expr → Value); `evaluate_messages` and `collect_messages` (messages mode); `collect_single_message`; `EvalContext` (call_stack, total_iterations, total_message_bytes, warnings, MAX_CALL_DEPTH=128, MAX_TOTAL_ITERATIONS=1_000_000, MAX_WARNINGS=1_000); `values_equal_runtime`; `condvalue_to_value`; `And`/`Or` short-circuit in `evaluate_condition`
- `crates/mds-core/src/validator.rs` — builtin-aware `validate_expr`; range arity checks; recursive `validate_condition`; `Node::Message` arm validates role + recurses body
- `crates/mds-core/src/resolver.rs` — orchestrator; `ModuleCache`; `process_module_messages`; `resolve_key_messages` / `resolve_source_messages` (take `base_dir: &str`); `FrontmatterImport` enum and parse functions; import semantics; security enforcement
- `crates/mds-core/src/lib.rs` — public API; `Message` struct; `CompileMessagesOutput` struct; `compile_messages*` function family; `scan_imports`; `load_vars_file`; `load_vars_str`; `check_virtual`; `compile_file`; `strip_reserved_keys` and `prepend_frontmatter`
- `crates/mds-cli/src/main.rs` — CLI: `OutputFormat` enum; `--format messages` on `build`; `read_build_input` (shared file reader with `MAX_FILE_SIZE` enforcement); `run_build`/`run_check`/`run_init`; `exit_code`; `resolve_output_path`; `load_config`
- `crates/mds-core/tests/messages.rs` — integration tests for `@message` / messages mode
- `crates/mds-cli/tests/format_messages.rs` — CLI integration tests for `--format messages`
- `crates/mds-core/tests/api_surface.rs` — public API regression tests; update when adding public symbols

## Related

- ADR-008: bundles related language features in single PR (applied to v0.2.0 — built-ins, default args, and logical operators shipped together; `@message` + `compile_messages` + `--format messages` shipped together in Issue #56)
- ADR-010: reuse `parse_expr_inner` across interpolation and directive parsing — `@message` role expressions (`{expr}` form) follow this same pattern; bare-word roles bypass `parse_expr_inner` and become `Expr::StringLiteral` directly
- `crates/mds-core/src/resolver.rs` — canonical reference for module system, import semantics, `FrontmatterImport`, messages-mode resolution, `Arc<ResolvedModule>` cache
- `crates/mds-core/src/evaluator.rs` — canonical reference for `EvalContext`, `evaluate_expr`, `evaluate_messages`, `collect_messages`, directive execution, closure restore, call-depth guards
- `crates/mds-core/src/scope.rs` — canonical reference for `CapturedScope`, `Arc<FunctionDef>`, closure capture API
- `crates/mds-core/src/ast.rs` — canonical reference for all AST types including `MessageBlock`; start here for new argument, expression, or directive forms
- `crates/mds-cli/tests/` — end-to-end tests across 10+ categorized files (`language.rs`, `objects.rs`, `imports.rs`, `errors.rs`, `cli_build.rs`, `cli_commands.rs`, `security.rs`, `frontmatter.rs`, `warnings.rs`, `format_messages.rs`) plus `common/mod.rs`
- Tech debt: issue #77 (ScanState extraction), #78 (CondValue/Expr unification), #79 (parse_interpolation_expr delegation), #80 (parse_simple_condition complexity)
