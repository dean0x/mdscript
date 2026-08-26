---
feature: mds-napi
name: MDS Native Node.js Bindings (mds-napi)
description: "Use when modifying the native addon API surface, adding new napi exports, debugging FFI marshaling, working on error serialization, understanding the discriminated-union wire format, or investigating why a JS caller gets unexpected result shapes. Keywords: mds-napi, napi-rs, compile, compileFile, check, checkFile, build_canonical_result, CheckResult, serde_json::Value, ToNapiValue, discriminated union, kind, output, messages, absent field, mds::mixed_content, mds::internal, mds::invalid_options, mds::resource_limit, throw_mds_error, run_catching, catch_unwind."
category: component-patterns
directories: ["crates/mds-napi/"]
referencedFiles:
  - crates/mds-napi/src/lib.rs
  - crates/mds-napi/Cargo.toml
  - crates/mds-napi/__test__/index.spec.mjs
  - crates/mds-napi/__test__/fixtures/messages.mds
  - crates/mds-napi/__test__/fixtures/mixed.mds
created: 2026-06-26
updated: 2026-06-26
---

# MDS Native Node.js Bindings (mds-napi)

## Overview

`crates/mds-napi/` is the native Node.js addon built with napi-rs. It exposes four `#[napi]` functions to JavaScript: `compile`, `compileFile`, `check`, and `checkFile`. All compilation logic lives in `mds-core`; the napi layer handles FFI marshaling, options parsing, resource-limit enforcement, and structured error throwing.

After the intrinsic-output refactor, `compile` and `compileFile` return a **discriminated union object** built field-by-field. The inactive payload field is structurally absent (no null injection). `check` and `checkFile` remain unchanged — they return `{ warnings: string[] }`.

## Core Responsibilities

- Expose `compile`, `compileFile`, `check`, `checkFile` to Node.js as native functions
- Build the canonical discriminated-union result object from `mds::CompileResult`
- Marshal Rust errors into JS errors with `.code`, `.help`, `.span` properties
- Enforce the 10 MiB source size limit at the napi boundary (before calling mds-core)
- Validate and parse the optional `opts` object (`basePath`, `vars` for `compile`; `vars` only for `compileFile`)
- Does NOT implement any compilation logic

## Standard Structure

### Exported napi functions

```rust
// Returns serde_json::Value (discriminated union)
#[napi] pub fn compile(env: Env, source: String, opts: Option<Object>) -> napi::Result<serde_json::Value>
#[napi(js_name = "compileFile")] pub fn compile_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<serde_json::Value>

// Returns CheckResult { warnings: Vec<String> }
#[napi] pub fn check(env: Env, source: String, opts: Option<Object>) -> napi::Result<CheckResult>
#[napi(js_name = "checkFile")] pub fn check_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<CheckResult>
```

### build_canonical_result — the wire-format builder

This private function is the single point of truth for the JS wire format:

```rust
fn build_canonical_result(result: mds::CompileResult) -> serde_json::Value {
    // Field-by-field construction: inactive field is ABSENT, not null
    match result.output {
        mds::CompiledOutput::Markdown(text) => serde_json::json!({
            "kind": "markdown",
            "output": text,       // "output" key, NOT "value"
            "warnings": warnings,
            "dependencies": dependencies,
        }),
        mds::CompiledOutput::Messages(msgs) => serde_json::json!({
            "kind": "messages",
            "messages": messages, // "messages" key, NOT "value"
            "warnings": warnings,
            "dependencies": dependencies,
        }),
    }
}
```

Key design decisions:
- `serde_json::Value` implements `ToNapiValue` in napi-rs 3.x via the `serde-json` feature. This is the correct owned return type for dynamic objects — it avoids `Object<'env>` lifetime issues.
- The `serde_json::json!()` macro lists fields explicitly — no serde derive inference that might inject extra fields.
- The payload key for markdown is `output` (not `value`) and for messages is `messages` (not `value`). This differs from the Rust-level adjacently-tagged serde shape (`content`/`value`).

### Wire shapes

Markdown:
```json
{ "kind": "markdown", "output": "<string>", "warnings": [], "dependencies": [] }
```

Messages:
```json
{ "kind": "messages", "messages": [{"role":"...","content":"..."}], "warnings": [], "dependencies": [] }
```

Check result (unchanged):
```json
{ "warnings": [] }
```

### Deleted exports

These must not be re-exposed:
- `compileMessages(source, opts)` — deleted
- `compileMessagesFile(path, opts)` — deleted
- `CompileMessagesResult` struct — deleted

The napi test spec has `AC-API-05` tests that assert `typeof addon.compileMessages === 'undefined'` and same for `compileMessagesFile`.

### Error codes

Errors thrown at the napi boundary carry a `.code` property:

| Code | Source |
|------|--------|
| `mds::*` (e.g. `mds::syntax`, `mds::mixed_content`) | from `mds-core` via `throw_mds_error` |
| `mds::internal` | napi-only; unexpected panic caught by `run_catching` |
| `mds::invalid_options` | napi-only; malformed options object |
| `mds::resource_limit` | napi-only; source string exceeds 10 MiB |

The `throw_mds_error` path serializes `MdsError` and attaches `.help` and `.span` properties using raw N-API calls (`raw_create_error`, `raw_set_string_prop`).

### Panic safety

All `#[napi]` functions wrap their `mds-core` call in `run_catching` + `catch_unwind`. The workspace panic strategy must remain `unwind` (see `CLAUDE.md` gotchas). The `debug-panics` Cargo feature gates leak of panic details — it must never ship enabled.

## Dependency Patterns

`Cargo.toml` must have `serde_json = { workspace = true, features = ["std"] }` and the napi `serde-json` feature enabled for `serde_json::Value` to implement `ToNapiValue`.

Options parsing uses direct napi property access (`obj.get_named_property_unchecked`) rather than bulk deserialization to give precise error messages for each invalid field. `reject_unknown_napi_keys` enumerates all keys and reports all unknown ones at once.

## Error Handling

`check` / `checkFile` now run full intrinsic dispatch via `mds-core`. They will return `mds::mixed_content` for templates with orphan content alongside `@message` blocks. This is a behavior change from before the refactor.

`compile` / `compileFile` accept `vars` with special characters; these round-trip byte-identical through FFI in both messages and markdown modes (K-VARS-1 test).

## Anti-Patterns

- **Returning `Object<'env>` from `#[napi]` for dynamic objects** — use `serde_json::Value` instead; `Object<'env>` has lifetime issues that prevent returning the constructed object
- **Using serde derive on the napi return type** — the wire format has different key names from the Rust adjacently-tagged shape; always use `build_canonical_result` with explicit `json!()` fields
- **Exposing `compileMessages` or `compileMessagesFile`** — deleted; assertions in test spec guard against re-adding them
- **Constructing `CheckResult` with a `dependencies` field** — `CheckResult` has only `warnings`; no deps are returned from check operations

## Gotchas

- The napi-generated `index.d.ts` is git-ignored; it is regenerated in CI. If you need to inspect the current type shape, check `crates/mds-napi/__test__/index.spec.mjs` assertions or `packages/mds/src/types.ts` (which mirrors the intended shapes).
- `basePath` option is accepted by `compile`/`check` but rejected by `compileFile`/`checkFile` (base directory is derived from the file path). The `parse_file_opts` function explicitly checks for and rejects `basePath`.
- Empty `basePath` string is rejected (`throw_options_error`).
- The `MAX_SOURCE_SIZE` constant at the napi boundary mirrors `mds::MAX_FILE_SIZE` because string-based `compile` calls bypass the file layer.

## Key Files

- `crates/mds-napi/src/lib.rs` — the entire napi implementation; `build_canonical_result`, all `#[napi]` exports, error helpers
- `crates/mds-napi/Cargo.toml` — must have `serde-json` feature for napi-rs and `serde_json` workspace dep
- `crates/mds-napi/__test__/index.spec.mjs` — 65 JS tests; "intrinsic output shape" group (K-MD-*, K-MSG-*, K-MIXED-*, K-VARS-*); AC-API-05 deletion assertions

## Related

- Feature: mds-compiler — `mds::CompileResult`, `mds::CompiledOutput`, `mds::compile_with_deps` are the core inputs to `build_canonical_result`
- Feature: mds-js — the JS package that re-exports these shapes as TypeScript types; `MarkdownResult`/`MessagesResult` union matches the wire format exactly
- Feature: bundler-plugins — also consumes `compileFile` return shape via `MdsApi` interface
