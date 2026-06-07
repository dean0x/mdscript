---
feature: mds-napi
name: MDS Native Node.js Bindings (napi-rs)
description: "Use when adding new exports to the native Node.js addon, changing error codes or error shape, working with N-API raw system calls, updating options parsing, or investigating the panic-safety boundary. Keywords: napi-rs, native addon, node, N-API, napi_create_error, catch_unwind, cdylib, mds-napi, @mdscript/mds-napi, bindings, options.rs, VarsError, parse_json_vars, BuiltinError, builtin_type_error, compileMessages, Message, CompileMessagesResult, messages mode."
category: component-patterns
directories: [crates/mds-napi/]
referencedFiles:
  - crates/mds-napi/src/lib.rs
  - crates/mds-napi/index.js
  - crates/mds-napi/Cargo.toml
  - crates/mds-napi/build.rs
  - crates/mds-napi/package.json
  - crates/mds-napi/__test__/index.spec.mjs
  - crates/mds-napi/__test__/fixtures/simple.mds
  - crates/mds-napi/__test__/fixtures/import_consumer.mds
  - crates/mds-napi/__test__/fixtures/import_provider.mds
  - crates/mds-core/src/lib.rs
  - crates/mds-core/src/options.rs
  - Cargo.toml
created: 2026-05-20
updated: 2026-06-07
---

# MDS Native Node.js Bindings (napi-rs)

## Overview

`mds-napi` is the native Node.js addon for the MDS compiler, compiled as a `cdylib` using napi-rs. It is published as `@mdscript/mds-napi` on npm. It bridges the Rust compiler in `mds-core` into the Node.js runtime by exposing five synchronous functions — `compile`, `compileFile`, `check`, `checkFile`, and `compileMessages` — with a structured error contract that the JavaScript side can discriminate by error code.

The crate sits at the boundary between Rust and JavaScript. Its primary concerns are: options parsing (converting a JS object to Rust types), error translation (converting `MdsError` into structured JS exceptions with a `code` property), panic safety (`catch_unwind` at every public entry point), and resource limits (re-enforcing `MAX_FILE_SIZE` for string inputs that bypass the file resolver).

## Core Responsibilities

- **Expose** exactly five `#[napi]`-decorated public functions: `compile`, `compileFile`, `check`, `checkFile`, `compileMessages`.
- **Parse** JS options objects using direct N-API property access (not full serde deserialization of the top-level object), then delegate vars parsing to the shared `mds::parse_json_vars` in `mds-core`.
- **Translate** `mds::MdsError` into a JS `Error` whose `.code` property is set via raw `napi_create_error`, with optional `.help` and `.span` extra properties.
- **Catch panics** using `catch_unwind` wrapped in `run_catching`, converting panics to `mds::internal` coded errors.
- **Enforce** the 10 MiB source size limit on string inputs; file-path inputs inherit the limit from `mds-core`'s resolver.
- **NOT** accept a virtual filesystem — this crate always compiles against the real OS filesystem.

## Exported Functions and Return Types

### compile / compileFile

- **Input**: source string (or file path) + optional `{ basePath?, vars? }` options.
- **Returns**: `CompileResult { output: String, warnings: Vec<String>, dependencies: Vec<String> }`.
- `compileFile` does not accept `basePath` (derived from the file's own directory).

### check / checkFile

- **Input**: source string (or file path) + optional options.
- **Returns**: `CheckResult { warnings: Vec<String> }`.
- Validates the template without producing output.

### compileMessages (added in Issue #56)

- **Input**: source string + optional `{ basePath?, vars? }` options (same option set as `compile`).
- **Returns**: `CompileMessagesResult { messages: Vec<Message>, warnings: Vec<String>, dependencies: Vec<String> }`.
- Compiles an MDS template in **messages mode**: each `@message role:` ... `@end` block becomes one `Message { role: String, content: String }` entry.
- Orphan text outside `@message` blocks is ignored with a warning.
- Empty messages (after trimming) are silently skipped.
- Throws an error when the template contains no `@message` blocks at all.
- Calls `mds::compile_messages_str_with_deps` from `mds-core` inside `run_catching`.
- Applies `check_source_size` and `parse_compile_opts` — identical guard/parse steps to `compile`.

### #[napi(object)] structs

| Struct | Fields | Used by |
|---|---|---|
| `CompileResult` | `output`, `warnings`, `dependencies` | `compile`, `compileFile` |
| `CheckResult` | `warnings` | `check`, `checkFile` |
| `CompileMessagesResult` | `messages`, `warnings`, `dependencies` | `compileMessages` |
| `Message` | `role`, `content` | nested in `CompileMessagesResult` |

**Important**: The message item type is named `Message` (not `MessageItem`). It was renamed in commit `dc3e0ea` (Issue #56) for cross-layer consistency with `mds-core::Message`. All struct fields must be `pub` — napi-rs requires this for `#[napi(object)]` codegen.

## Standard Structure

All five exported functions follow an identical three-step pattern:

1. **Guard** — check source size (string variants only) or validate that `basePath` is absent (file variants).
2. **Parse options** — validate the JS options object via `parse_compile_opts` or `parse_file_opts`, which use direct N-API property access helpers.
3. **Run catching** — call the corresponding `mds-core` function inside `run_catching`, which wraps the call in `catch_unwind` and maps both `MdsError` and panics to structured JS exceptions.

`parse_compile_opts` returns `CompileOpts = (Option<PathBuf>, Option<HashMap<String, Value>>)` — a simple tuple alias, not a named struct.

The two options parsers enforce different allowed key sets:

- `parse_compile_opts` (for `compile`, `check`, and `compileMessages`): accepts `basePath` and `vars`.
- `parse_file_opts` (for `compileFile` and `checkFile`): accepts `vars` only; explicitly rejects `basePath` with a helpful message. Returns `Option<HashMap<String, Value>>` directly.

Both parsers call `reject_unknown_napi_keys` to catch unknown keys early, surfacing misspelled option names as `mds::invalid_options` errors rather than silently ignoring them.

## Options Parsing Architecture

Options parsing uses a two-layer approach that avoids deserializing the entire options object through serde:

**Layer 1 — NAPI direct access** (in `mds-napi/src/lib.rs`): Helpers that work directly with the N-API `Object` type to enumerate keys and read individual properties.

- `napi_type_name(vt: ValueType)` — maps a `napi::ValueType` to a human-readable string for error messages.
- `reject_unknown_napi_keys(env, obj, known)` — enumerates all property names via `get_property_names()`, deserializes that Array as JSON, filters out known keys, and reports all unknown keys at once using `format_unknown_keys_error` from `mds-core`.
- `extract_base_path_direct(env, obj)` — reads `basePath` as a typed `Unknown` value, checks its `ValueType`, and returns `None` for absent/null/undefined, `Some(PathBuf)` for valid non-empty strings, or an error for wrong types.
- `extract_vars_direct(env, obj)` — reads `vars` as a typed `Unknown` value; for `ValueType::Object`, deserializes only that sub-value to `serde_json::Value` and delegates to `mds::parse_json_vars`; for other types, errors with `napi_type_name`.

**Layer 2 — Shared JSON utilities** (in `mds-core/src/options.rs`, re-exported from `mds-core`):

- `mds::json_type_name(v: &serde_json::Value)` — type name for JSON values, used in diagnostic messages.
- `mds::parse_json_vars(vars_value: serde_json::Value)` — validates that the value is a JSON object (not array, string, etc.) and converts entries to `HashMap<String, mds::Value>`. Returns `VarsError::InvalidType` for wrong types or `VarsError::Conversion` for values that can't be converted.
- `mds::format_unknown_keys_error(unknowns: &[&str], known: &[&str])` — builds the singular/plural "unknown option key(s)" message. Used by both `reject_unknown_napi_keys` (napi layer) and `reject_unknown_json_keys` (shared JSON layer).
- `mds::reject_unknown_json_keys(map, known)` — validates a `serde_json::Map` against allowed keys. Used by mds-wasm, not by mds-napi (napi uses `reject_unknown_napi_keys` instead).
- `mds::VarsError` — error type returned by `parse_json_vars`, with variants `InvalidType(String)` and `Conversion(MdsError)`.

The key design insight: `extract_vars_direct` only serializes the `vars` sub-value, not the entire options object. This keeps the serde boundary narrow and lets the NAPI layer use typed `ValueType` checks for the top-level keys.

## Dependency Patterns

```toml
# crates/mds-napi/Cargo.toml

[lib]
crate-type = ["cdylib"]    # required for a native .node file

[dependencies]
mds = { package = "mds-core", path = "../mds-core", version = "0.2.0" }
napi = { workspace = true }        # napi3 + serde-json features
napi-derive = { workspace = true } # #[napi] procedural macro
serde_json = { workspace = true }  # used for vars sub-value deserialization

[build-dependencies]
napi-build = { workspace = true }  # generates the module registration boilerplate

[features]
debug-panics = []   # exposes raw panic payload on mds::internal — NEVER enable in production
```

Key points: `napi` is declared at workspace level with `features = ["napi3", "serde-json"]`. The `serde-json` feature enables `env.from_js_value(val)` to deserialize a single JS value to `serde_json::Value`. The workspace-level `[profile.release]` forces `panic = "unwind"` because `catch_unwind` requires the unwind ABI.

**Version pinning**: The `version` field in the path+version dep on `mds-core` must be updated whenever the workspace version is bumped. For pre-1.0 semver, `^0.1.0` does NOT satisfy `0.2.0` — `bump-version.mjs` handles this automatically as of v0.2.0, but hand-editing `Cargo.toml` requires explicit version sync.

The shared options utilities (`parse_json_vars`, `format_unknown_keys_error`, `VarsError`, `json_type_name`, `reject_unknown_json_keys`) are imported from `mds` (the re-export alias for `mds-core`) — not duplicated in this crate.

## Error Handling

### The Error Code Contract

Every JS error thrown by this crate carries a `code` string. The codes defined by `mds-core` (e.g. `mds::syntax`, `mds::undefined_var`, `mds::file_not_found`) pass through unchanged. Three additional codes are synthesised only at the napi boundary:

| Code | Origin | Meaning |
|---|---|---|
| `mds::internal` | napi boundary | Rust panic caught by `catch_unwind` |
| `mds::invalid_options` | napi boundary | Malformed or type-incorrect JS options object |
| `mds::resource_limit` | napi boundary | Source string exceeds 10 MiB |

Two additional codes originate in `mds-core` and pass through the napi boundary unchanged:

| Code | Origin | Meaning |
|---|---|---|
| `mds::builtin_type_error` | mds-core builtins module | A built-in function was called with an argument of the wrong type |
| `mds::arity_mismatch` | mds-core evaluator | A function was called with the wrong number of arguments |

`MdsError` is `#[non_exhaustive]`, so new error variants can be added to `mds-core` without a breaking change to this crate. The `throw_mds_error` helper maps all `MdsError` variants by code string, so new codes from `mds-core` flow through to JS automatically.

### ArityMismatch Error Message Format

`MdsError::ArityMismatch` reports argument count requirements as a range when a function accepts a variable number of arguments. The `.message` on the thrown JS error uses the format `"expected 1-3 arguments, got 5"` rather than a single-value format. Code that matches on the error message string (rather than the `.code` property) may need to be updated. Always discriminate on `.code`, not on the message text.

### Why Raw N-API for Error Creation

napi-rs's high-level `napi::Error` type does not support setting the `.code` property on the underlying JS `Error` object. To attach a machine-readable `code`, `help`, and `span`, the crate bypasses napi-rs and calls `napi_create_error` directly via `napi::sys`. The return convention is to call `napi_throw(env, err_obj)` and then return `napi::Error::new(Status::PendingException, "")` — the `PendingException` sentinel tells napi-rs that a JS exception is already pending and it must not create a second one.

The helper functions are structured as follows: `raw_create_error` creates the `Error` with code; `raw_set_string_prop` and `raw_set_uint32_prop` attach extra properties; `throw_mds_error` orchestrates both for `MdsError`; `throw_coded_error` handles the boundary-only codes.

All raw N-API calls are `unsafe`. The invariants are: `env` is valid for the duration of the call (guaranteed by napi-rs), and values are used before any allocating re-entrant call can invalidate them.

### VarsError Mapping

`extract_vars_direct` maps `VarsError` variants to distinct napi errors:

- `VarsError::InvalidType(msg)` → `throw_options_error(env, &msg)` (code `mds::invalid_options`)
- `VarsError::Conversion(mds_err)` → `throw_mds_error(env, mds_err)` (uses the error code from `mds-core`)

### Span Shape

When an `MdsError` carries source location information, `throw_mds_error` creates a `span` JS object with `{ offset: u32, length: u32, line?: u32, column?: u32 }`. Both `line` and `column` are optional (they are `Option<usize>` in the serialized form) and are omitted from the span object when absent. Tests validate the shape in test group `E-8`.

## Integration Guidelines

### Adding a New Exported Function

1. Define the Rust function with `#[napi]` (or `#[napi(js_name = "camelCaseName")]` for non-snake-case names).
2. Accept `env: Env` as the first argument — required for error construction helpers.
3. Apply `run_catching` around any call into `mds-core` to maintain panic safety.
4. For string-source variants, call `check_source_size` before parsing options.
5. Define which option keys are valid. If the set differs from existing parsers, add a new `parse_*_opts` function that calls `reject_unknown_napi_keys`, `extract_base_path_direct`, and/or `extract_vars_direct`.
6. Return types exposed to JS must use `#[napi(object)]`. Struct fields must be `pub`.

### Calling mds-core Functions

The napi layer calls the `*_with_deps` / `*_collecting_warnings` family of functions exclusively (e.g. `mds::compile_str_with_deps`, `mds::compile_with_deps`, `mds::check_str_collecting_warnings`, `mds::check_collecting_warnings`, `mds::compile_messages_str_with_deps`). These return warnings as a `Vec<String>` in the return value rather than printing to stderr, so the addon can surface them in the JS return value. Never call the `emit_warnings` variants from the addon — they write to stderr and the warnings would disappear from the JS caller's perspective.

The public signatures of these functions have NOT changed: string-source variants still accept `Option<&Path>` for `base_dir`, and file-path variants still accept `impl AsRef<Path>`. Internal changes to `resolve_base_dir` (now returns `String`) and `ModuleCache::resolve_path`/`resolve_source` (now take `&str` instead of `&Path`) are transparent to the napi layer — they are private implementation details inside `mds-core/src/lib.rs`.

### Extending Options Parsing

When adding a new option key:

1. Add the key name to the `known` slice passed to `reject_unknown_napi_keys`.
2. Add a new `extract_*_direct` helper following the `extract_base_path_direct` / `extract_vars_direct` pattern: read via `get_named_property_unchecked`, check `get_type()`, handle `Undefined`/`Null` as absent, and error with `napi_type_name(other)` for unexpected types.
3. Do NOT deserialize the full options object through serde — keep the boundary narrow to the specific sub-value being extracted.

## Anti-Patterns

- **Returning `napi::Error::new(Status::GenericFailure, ...)` directly** — this creates a plain JS `Error` without a `.code` property. Always go through `throw_mds_error` or `throw_coded_error` so that consumers can discriminate errors by code.
- **Calling `env.throw_error(msg, code)` on the happy path** — the `env.throw_error` fallback inside `raw_create_error` is only for the rare case where the raw N-API call itself fails (null pointer returned). Use `throw_coded_error` as the primary path.
- **Using the non-`_collecting_warnings` mds-core functions** — those emit warnings to stderr and return `Result<String, MdsError>`, not `(output, warnings)`. Warnings would never reach the JS `result.warnings` array.
- **Enabling the `debug-panics` feature outside local dev** — the raw panic payload leaks absolute filesystem paths from the build machine, which is a security/privacy issue in shipped binaries.
- **Passing `basePath` to `compileFile`/`checkFile`** — the file variants derive their base directory from the file path itself; accepting `basePath` would create ambiguity. The parser explicitly rejects it with a descriptive error message.
- **Forgetting `AssertUnwindSafe`** — closures passed to `run_catching` / `catch_unwind` must be `UnwindSafe`. Closures that capture `String` or `PathBuf` need `AssertUnwindSafe(move || {...})` because those types are not `UnwindSafe` by default.
- **Deserializing the full options object with serde** — the old approach serialized the entire `Object` to `serde_json::Value` then removed known keys. The current approach reads individual properties directly, keeping serde deserialization limited to the `vars` sub-value only.
- **Duplicating `parse_json_vars`, `json_type_name`, or `format_unknown_keys_error` in this crate** — these now live in `mds-core/src/options.rs` and are shared with `mds-wasm`. If you need to change the error message format or vars validation logic, change it there.
- **Matching on error message text instead of `.code`** — message formats are not part of the public contract and can change (e.g. `ArityMismatch` now uses a range format). Always branch on `.code`.
- **Hand-editing the `version` in path+version deps on `mds-core`** — use `bump-version.mjs` to keep all versions in sync. Pre-1.0 semver does not allow cross-minor satisfying (`^0.1.0` does not satisfy `0.2.0`).
- **Running `napi build` without `--no-js` in the mds-napi crate** — see the A3 gate gotcha below. The auto-generated loader references wrong package names and wrong `.node` filenames and will clobber the hand-maintained `index.js`.
- **Referring to the message item struct as `MessageItem`** — the struct was renamed to `Message` in Issue #56 (commit `dc3e0ea`) for cross-layer consistency with `mds-core::Message`. The old name no longer exists.

## Gotchas

- **`panic = "unwind"` is workspace-global.** The workspace `Cargo.toml` sets `panic = "unwind"` in both `[profile.dev]` and `[profile.release]` because Cargo does not support per-crate panic strategies within a workspace. This affects every crate in the workspace, not just mds-napi.
- **`null` and `undefined` options are both valid.** The `opts: Option<Object>` napi-rs parameter maps both JS `null` and JS `undefined` to `None`. The test suite explicitly covers both cases (F-C2, F-C3). Do not add special-case handling for `null` — napi-rs handles the coercion.
- **`basePath: null` is silently treated as absent.** Inside `extract_base_path_direct`, `ValueType::Null` is mapped to `None` (same as omitting the key), rather than raising an error. This matches the JS convention where `null` means "not provided".
- **`ValueType::Object` includes JS arrays.** When `extract_vars_direct` sees `ValueType::Object`, it cannot distinguish a plain object from an array at the N-API type level — both report as `Object`. The distinction is made inside `parse_json_vars`: serde deserializes a JS array as `serde_json::Value::Array`, and the `let Value::Object(map) else` guard in `parse_json_vars` rejects it with `VarsError::InvalidType`.
- **Source size limit is re-enforced at the napi boundary.** The `mds-core` resolver enforces `MAX_FILE_SIZE` for file reads. When a caller passes source as a string via `compile`, `check`, or `compileMessages`, the file resolver is bypassed. `check_source_size` re-applies the same limit using `mds::MAX_FILE_SIZE` as the single source of truth, so the limit stays synchronized when `mds-core` changes it.
- **`resolve_base_dir` now returns `String`, not `PathBuf`.** As of the unified backend refactor, the private `resolve_base_dir` helper in `mds-core/src/lib.rs` converts `Option<&Path>` directly to a UTF-8 `String` (failing explicitly on non-UTF-8 paths). `ModuleCache::resolve_source` correspondingly takes `&str` for path arguments instead of `&Path`. This is transparent to the napi layer because it calls the stable public wrappers (`compile_with_deps`, etc.), but matters if you read resolver internals.
- **Test runner requires Node.js 22+.** Tests use the built-in `node:test` runner. Running them with Node 18 or 20 will fail with import errors or missing test API features.
- **The built `.node` binary must exist before running tests.** Tests load `../mds-napi.node` directly via `require`. The file is produced by `cargo build --release` plus `napi-rs CLI`. Tests cannot be run from source alone.
- **The napi test suite does not yet test `compileMessages`.** The `index.spec.mjs` currently only destructures `{ compile, compileFile, check, checkFile }` from the addon. `compileMessages` is exercised through the JS package (`@mdscript/mds`) integration tests against the WASM backend, not directly in the napi test file.
- **Dependency paths in `CompileResult` are absolute when using `compileFile`.** The `dependencies` field contains paths as returned by `mds-core`'s module cache, which normalizes them to absolute paths. For `compile` (source string variant), dependencies are also absolute if the provider files are resolved from an absolute `basePath`.
- **`MdsError` is `#[non_exhaustive]`.** New variants (e.g. `BuiltinError` and `ArityMismatch`) do not break the napi layer's `throw_mds_error` — it maps errors by their code string. However, exhaustive match arms on `MdsError` in any future helper code will fail to compile when new variants are added.
- **npm package name is `@mdscript/mds-napi`, not `mds-napi`.** The crate is named `mds-napi` but the published npm host package is `@mdscript/mds-napi`. Platform packages are `@mdscript/mds-napi-{platform}`. The `node.ts` loader uses `require('@mdscript/mds-napi')`.
- **Path+version dep on mds-core requires explicit version sync.** When the workspace version bumps, `mds = { package = "mds-core", path = "../mds-core", version = "X.Y.Z" }` must reflect the new version. `bump-version.mjs` handles this since v0.2.0.
- **A3 name-gate: `napi build` without `--no-js` clobbers the hand-maintained `index.js` loader.** `crates/mds-napi/index.js` is a 49-line hand-maintained file — it is NOT auto-generated. During Issue #56 development, running `napi build` (without `--no-js`) regenerated this file with auto-generated content that referenced wrong package names (e.g. `undefined-<triple>` instead of `@mdscript/mds-napi-<triple>`) and wrong `.node` filenames, causing the A3 name-gate (`scripts/verify-napi-names.mjs`) to fail. The file had to be restored from version control. Rules: (1) always pass `--no-js` when invoking `napi build` inside `crates/mds-napi/`, or (2) restore `index.js` immediately afterward from git before running the A3 gate. The gate (`scripts/verify-napi-names.mjs`) checks that the loader's platform-package names and `.node` filenames match what `napi create-npm-dirs` would generate — drift causes silent binary load failures at runtime on every platform. The gate is a hard checkpoint in the release workflow; do not proceed past a failing gate.

## Key Files

- `crates/mds-napi/src/lib.rs` — entire implementation: all five exports (`compile`, `compileFile`, `check`, `checkFile`, `compileMessages`), error helpers, options parsers, `run_catching`, size guard, and all `#[napi(object)]` structs (`CompileResult`, `CheckResult`, `CompileMessagesResult`, `Message`). `CompileOpts` is a private type alias `(Option<PathBuf>, Option<HashMap<String, Value>>)`.
- `crates/mds-napi/index.js` — hand-maintained 49-line platform-dispatch loader. NOT auto-generated. Maps `platform`+`arch`+libc to the correct `.node` filename and `@mdscript/mds-napi-<triple>` package name. Must not be regenerated by `napi build` without `--no-js`.
- `crates/mds-core/src/options.rs` — shared options utilities: `json_type_name`, `parse_json_vars`, `format_unknown_keys_error`, `reject_unknown_json_keys`, `VarsError`. Re-exported from `mds-core` for use by both `mds-napi` and `mds-wasm`.
- `crates/mds-napi/Cargo.toml` — crate manifest; declares `cdylib` type, `debug-panics` feature, workspace dependency pins, and the path+version dep on `mds-core`.
- `crates/mds-napi/build.rs` — single call to `napi_build::setup()`, generates module registration.
- `crates/mds-napi/package.json` — npm package metadata (`@mdscript/mds-napi`) used by `@napi-rs/cli` for binary distribution.
- `crates/mds-napi/__test__/index.spec.mjs` — integration test suite (Node.js 22+, `node:test`), covers compile/compileFile/check/checkFile plus error shape and resource limits. Does not yet include `compileMessages` tests.
- `crates/mds-core/src/lib.rs` — public API surface that napi bridges; `compile_with_deps`, `compile_str_with_deps`, `check_collecting_warnings`, `check_str_collecting_warnings`, `compile_messages_str_with_deps` are the functions called by the addon. Defines `MdsError` (`#[non_exhaustive]`).
- `scripts/verify-napi-names.mjs` — A3 gate; verifies the hand-written loader's platform-package names and `.node` filenames match generated platform package names. Run in CI at stage-and-verify-napi and publish-npm steps.
- `Cargo.toml` (workspace) — defines `panic = "unwind"` profiles and workspace-level napi dependency versions.

## Related

- `crates/mds-core/src/options.rs` — defines the shared options utilities imported by this crate. Changes to `parse_json_vars` or `format_unknown_keys_error` affect both mds-napi and mds-wasm.
- `crates/mds-core/src/lib.rs` — defines `MdsError`, `CompileOutput`, `VarsError`, `MessagesOutput`, and the `*_collecting_warnings` functions that the addon calls.
- `crates/mds-wasm/` — parallel WASM binding for the same compiler; uses `wasm-bindgen` instead of napi-rs but shares the same `mds-core::options` utilities and applies the same `catch_unwind` pattern at the boundary. Compare when making changes that affect both targets.
- `scripts/bump-version.mjs` — handles path+version dep updates for `mds-core` across `mds-cli`, `mds-napi`, and `mds-wasm` Cargo.toml files. Required because `^0.X.Y` in pre-1.0 semver does not satisfy the next minor version.
- `scripts/verify-napi-names.mjs` — A3 gate; avoids the pitfall where the hand-written loader drifts from generated platform packages, which silently breaks native binary loading at runtime on every affected platform.
- A3 name-gate pitfall (DECISIONS_CONTEXT) — the hand-written `crates/mds-napi/index.js` loader must not drift from the generated platform packages (`@mdscript/mds-napi-<triple>` / `mds-napi.<triple>.node`). Drift silently breaks native binary loading at runtime. Observed concrete failure: `napi build` without `--no-js` during Issue #56 regenerated the loader with `undefined-<triple>` package names, failing the gate. Always use `--no-js` or restore from git immediately after build.
