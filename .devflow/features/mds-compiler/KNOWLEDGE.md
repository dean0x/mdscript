---
feature: mds-compiler
name: MDS Compiler Core (mds-core)
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, working with @message blocks, the intrinsic output format, CompiledOutput, CompileResult, or mixed-content errors. Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, include, define, for, if, message, @message, CompiledOutput, CompileResult, into_markdown, into_messages, intrinsic, mixed_content, MixedContent, has_message_block, process_module_intrinsic, collect_messages_strict, evaluate_messages_intrinsic, TextNode.offset."
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
created: 2026-06-26
updated: 2026-06-26
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

## Related

- Feature: mds-cli — consumes `CompileResult` and `CompiledOutput`; derives output extension from `OutputKind::from(&compiled.output)`
- Feature: mds-napi — builds the canonical discriminated-union wire object from `CompileResult` via `build_canonical_result`
- Feature: mds-js — TypeScript `CompileResult = MarkdownResult | MessagesResult` union mirrors this Rust type
