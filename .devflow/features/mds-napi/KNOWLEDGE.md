---
feature: mds-napi
name: MDS Native Node.js Bindings (mds-napi)
description: "Use when modifying the native addon API surface, adding new napi exports, debugging FFI marshaling, working on error serialization, understanding the discriminated-union wire format, changing rules/severity option parsing, or investigating why a JS caller gets unexpected result shapes or an uncoded error. Keywords: mds-napi, napi-rs, compile, compileFile, check, checkFile, lint, lintFile, lintVirtual, build_canonical_result, to_canonical_json, CheckResult, serde_json::Value, ToNapiValue, discriminated union, kind, output, messages, absent field, mds::mixed_content, mds::internal, mds::invalid_options, mds::resource_limit, throw_mds_error, run_catching, catch_unwind, parse_rule_severities, Severity::from_str, format_unknown_keys_error, from_js_value."
category: component-patterns
directories: ["crates/mds-napi/"]
referencedFiles:
  - crates/mds-napi/src/lib.rs
  - crates/mds-napi/Cargo.toml
  - crates/mds-napi/README.md
  - crates/mds-napi/__test__/index.spec.mjs
  - crates/mds-napi/__test__/fixtures/messages.mds
  - crates/mds-napi/__test__/fixtures/mixed.mds
created: 2026-06-26
updated: 2026-09-28
---

# MDS Native Node.js Bindings (mds-napi)

## Overview

`crates/mds-napi/` is the native Node.js addon built with napi-rs. It exposes **seven** `#[napi]` functions to JavaScript: `compile`, `compileFile`, `check`, `checkFile`, `lint`, `lintFile`, and `lintVirtual`. All compilation and lint logic lives in `mds-core`; the napi layer handles FFI marshaling, options parsing, resource-limit enforcement, and structured error throwing. (The crate's own module-level rustdoc at the top of `src/lib.rs` still lists only the four compile/check exports — it predates the lint exports and is stale; don't trust it for the current surface, trust the `#[napi]` function list below.)

`compile`/`compileFile` and `lint`/`lintFile`/`lintVirtual` all build their return value by calling **`.to_canonical_json()`** on the core result type (`mds::CompileResult` / `mds::LintResult`) rather than constructing the JSON by hand. This is the single authoritative wire-format implementation shared byte-for-byte with the WASM binding (AC-API-13). `build_canonical_result` is now a thin one-line wrapper around `CompileResult::to_canonical_json()` — the wire shape itself (field names, absent-vs-null policy) is defined in `mds-core`, not in this crate. Lint results additionally run through `mds::attach_lint_warnings` to splice in the binding-only `lint_warnings` field. For the lint JSON shape and rule/severity/fix-edit semantics in depth, see the **mds-lint** feature knowledge — this file covers only the napi-specific FFI plumbing around it.

## Core Responsibilities

- Expose `compile`, `compileFile`, `check`, `checkFile`, `lint`, `lintFile`, `lintVirtual` to Node.js as native functions
- Delegate wire-format construction to `mds::CompileResult::to_canonical_json` / `mds::LintResult::to_canonical_json` — never build the JSON object by hand at this layer
- Marshal Rust errors into JS errors with `.code`, `.help`, `.span` properties
- Enforce the 10 MiB source size limit at the napi boundary (before calling mds-core), plus `lintVirtual`'s own module-count (256) and aggregate-size caps
- Validate and parse the optional `opts` object per export (`basePath`, `vars` for `compile`/`check`; `vars` only for the `*File` variants; `vars` + `rules` for the lint exports)
- Parse every `rules` severity value through mds-core's shared `parse_rule_severities`/`Severity::from_str` — never re-implement severity parsing locally
- Does NOT implement any compilation or lint logic itself

## Standard Structure

### Exported napi functions

```rust
// Returns serde_json::Value (discriminated union)
#[napi] pub fn compile(env: Env, source: String, opts: Option<Object>) -> napi::Result<serde_json::Value>
#[napi(js_name = "compileFile")] pub fn compile_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<serde_json::Value>

// Returns CheckResult { warnings: Vec<String> }
#[napi] pub fn check(env: Env, source: String, opts: Option<Object>) -> napi::Result<CheckResult>
#[napi(js_name = "checkFile")] pub fn check_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<CheckResult>

// Returns serde_json::Value (canonical lint JSON, mds-lint feature covers the shape)
#[napi] pub fn lint(env: Env, source: String, opts: Option<Object>) -> napi::Result<serde_json::Value>
#[napi(js_name = "lintFile")] pub fn lint_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<serde_json::Value>
#[napi(js_name = "lintVirtual")] pub fn lint_virtual(env: Env, modules: serde_json::Value, entry: String, opts: Option<Object>) -> napi::Result<serde_json::Value>
```

### build_canonical_result — a thin delegate, not the wire-format authority

```rust
/// Delegates to `mds::CompileResult::to_canonical_json`, which is the single
/// authoritative implementation shared with the WASM binding (AC-API-13: both
/// bindings must produce byte-identical wire output).
fn build_canonical_result(result: mds::CompileResult) -> serde_json::Value {
    result.to_canonical_json()
}
```

Key design decisions:
- `serde_json::Value` implements `ToNapiValue` in napi-rs 3.x via the `serde-json` feature (`napi = { features = ["napi3", "serde-json"] }` at the workspace level). This is the correct owned return type for dynamic objects — it avoids `Object<'env>` lifetime issues.
- The wire shape (field names, which field is absent per variant) lives entirely in `mds-core`'s `to_canonical_json`. Do **not** reintroduce a local `serde_json::json!()` construction here — that was the pre-refactor design and is no longer how this crate builds results.
- The lint exports additionally call `mds::attach_lint_warnings(json.as_object_mut().expect(...), lint_warnings)` after `to_canonical_json()` to splice in the napi-only `lint_warnings: string[]` field when `opts.rules` named an unknown rule.

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

Lint result (see the **mds-lint** feature knowledge for the full diagnostic/fix-edit shape):
```json
{ "version": 1, "files": [{"file": "...", "diagnostics": [...]}], "truncated": false, "lint_warnings": ["..."] }
```
`lint_warnings` is present only when `opts.rules` named an unknown rule; an unknown *severity* value still throws `mds::invalid_options` rather than warning.

### Deleted exports

These must not be re-exposed:
- `compileMessages(source, opts)` — deleted
- `compileMessagesFile(path, opts)` — deleted
- `CompileMessagesResult` struct — deleted

The napi test spec has `AC-API-05` tests that assert `typeof addon.compileMessages === 'undefined'` and same for `compileMessagesFile`.

### Rules/severity parsing — one parser shared by all three bindings (#175, #418)

`extract_rules_direct` no longer parses each severity string itself. It deserializes the `rules` sub-object, then calls the shared core helper:

```rust
// mds-core parses every severity and words the error, the rule name and
// the value escaped, as it does for WASM and Python (#418).
let rules = mds::parse_rule_severities(rules_map, "options.rules")
    .map_err(|message| throw_options_error(env, &message))?;
```

- `mds::parse_rule_severities(rules, field)` reads every value through `Severity`'s `FromStr` impl and builds both possible error messages once, with the rule name and the offending value WIRE-escaped (control chars, DEL, bidi/format hazards escaped; TAB left raw). `field` is the caller's own label (`"options.rules"` on napi/WASM, `"rules"` on Python, `"lint.rules"` on the CLI's `mds.json` reader) and appears verbatim in the message.
- `Severity::from_str` is a **direct four-arm match** — exactly `off`, `info`, `warn`, `error` — with no case folding, no trimming, and critically **no escape decoding**. Before this change, napi and WASM validated a severity by wrapping it in quotes and parsing it as a JSON string, which decoded any embedded escape sequence *first* — so a value that spelled one of the four words using an escaped-unicode form for one of its letters was silently accepted as if it were the plain spelling. That decode-then-validate path is gone; only the four exact plain-ASCII spellings parse on any binding now. A value that used to sneak through that way is now `mds::invalid_options` (`unknown severity`), matching what Python already enforced.
- The unknown-severity and wrong-type messages are now **worded identically** across napi, WASM, and Python (a deliberate convergence decision made during the v0.5.0 Wave 1.5 review — previously each binding had slightly different wording, e.g. napi/WASM said `valid values are "off", "info", "warn", "error"` where the shared wording now says `expected "off", "info", "warn", or "error"`).
- `format_unknown_keys_error` (used by `reject_unknown_napi_keys`) is likewise a shared mds-core helper: an unknown option key name is now WIRE-escaped in the thrown message on every binding, not shown raw.

### Error codes

Errors thrown at the napi boundary carry a `.code` property:

| Code | Source |
|------|--------|
| `mds::*` (e.g. `mds::syntax`, `mds::mixed_content`, `mds::not_mds`) | from `mds-core` via `throw_mds_error` |
| `mds::internal` | napi-only; unexpected panic caught by `run_catching` |
| `mds::invalid_options` | napi-only; malformed options object, bad `rules`/`vars`/`modules` shape, unknown option/rule key |
| `mds::resource_limit` | napi-only; source string, a `lintVirtual` module, or the whole `modules` map exceeds its size/count cap |

The `throw_mds_error` path serializes `MdsError` and attaches `.help` and `.span` properties using raw N-API calls (`raw_create_error`, `raw_set_string_prop`).

### Panic safety

All `#[napi]` functions wrap their `mds-core` call in `run_catching` + `catch_unwind`. The workspace panic strategy must remain `unwind` (see `CLAUDE.md` gotchas). The `debug-panics` Cargo feature gates leak of panic details — it must never ship enabled.

## Dependency Patterns

The `serde-json` napi feature (needed for `serde_json::Value: ToNapiValue`) is enabled at the **workspace** level (`napi = { version = "...", default-features = false, features = ["napi3", "serde-json"] }` in the root `Cargo.toml`), not in `crates/mds-napi/Cargo.toml` itself, which only pulls in `napi`/`napi-derive`/`serde_json` via `workspace = true`.

Options parsing uses direct napi property access (`obj.get_named_property_unchecked`) rather than bulk deserialization to give precise error messages for each invalid field. `reject_unknown_napi_keys` enumerates all keys and reports all unknown ones at once, via the shared `format_unknown_keys_error`.

## Error Handling

`check` / `checkFile` run full intrinsic dispatch via `mds-core`. They return `mds::mixed_content` for templates with orphan content alongside `@message` blocks.

`compile` / `compileFile` accept `vars` with special characters; these round-trip byte-identical through FFI in both messages and markdown modes (K-VARS-1 test).

`compileFile`/`checkFile`/`lintFile` name a not-`.mds` entry (`mds::not_mds`) by the path **as the caller typed it**, never by its resolved canonical absolute path (#417) — regression test id is `F-CF8` in `index.spec.mjs` (renamed from `CF-NOTMDS` during the v0.5.0 Wave 1.5 review for consistency with the sibling wrapper/Python test ids `U-CF10` / `test_e3_not_mds_names_typed_path`).

## Anti-Patterns

- **Returning `Object<'env>` from `#[napi]` for dynamic objects** — use `serde_json::Value` instead; `Object<'env>` has lifetime issues that prevent returning the constructed object
- **Constructing the compile/check/lint wire object by hand with `serde_json::json!()`** — the canonical shape lives in `mds::CompileResult::to_canonical_json`/`mds::LintResult::to_canonical_json`; call that, don't reimplement it here (it would drift from the WASM binding, which shares the same core method)
- **Re-parsing a `rules` severity value locally** (quoting it and running it through a JSON-string parser, or hand-matching the four spellings) — always call `mds::parse_rule_severities`; a local reimplementation is exactly how the pre-#418 escape-decoding bug was introduced
- **Exposing `compileMessages` or `compileMessagesFile`** — deleted; assertions in test spec guard against re-adding them
- **Constructing `CheckResult` with a `dependencies` field** — `CheckResult` has only `warnings`; no deps are returned from check operations

## Gotchas

- The napi-generated `index.d.ts` is git-ignored; it is regenerated in CI from the `#[napi]` functions' rustdoc comments. If you need to inspect the current type shape or JSDoc wording (e.g. the `sources[0]` note on `compile`), check `crates/mds-napi/__test__/index.spec.mjs` assertions, the rustdoc comment on the export itself, or `packages/mds/src/types.ts` (which mirrors the intended shapes) — don't rely on a locally-generated `index.d.ts` staying in sync with a doc-comment edit until it's regenerated.
- **Local rebuild trap (PF-035).** `npx napi build --platform` writes a *platform-suffixed* addon (`mds-napi.<triple>.node`); the node `--test` harness `require()`s the base `mds-napi.node` by name and will silently keep exercising a stale binary if you build with `--platform`. Rebuild with `npm run build:native -w @mdscript/mds-napi` (`napi build --release --no-js`, no `--platform`) after any Rust change before trusting a local JS test run.
- `basePath` option is accepted by `compile`/`check` but rejected by `compileFile`/`checkFile`/`lintFile`/`lintVirtual` (base directory is derived from the file path, or is meaningless for virtual modules). The relevant `parse_*_opts` function explicitly checks for and rejects `basePath`.
- Empty `basePath` string is rejected (`throw_options_error`).
- The `MAX_SOURCE_SIZE` constant at the napi boundary mirrors `mds::MAX_FILE_SIZE` because string-based `compile`/`lint` calls bypass the file layer. `lintVirtual` additionally enforces its own `MAX_MODULE_COUNT` (256) and `MAX_MODULES_AGGREGATE_SIZE` (same 10 MiB ceiling) per-key and cumulatively, since the virtual-FS path bypasses the file layer's size guard entirely — these bounds are binding-local (mirrored on WASM/Python), not enforced in `mds-core`.
- **A severity spelling that decodes to a valid word via an escaped-unicode letter is no longer accepted (#175, #418) — this is a breaking behavior change.** Only the exact plain-ASCII spellings `off`/`info`/`warn`/`error` parse; anything else, including a spelling that would have decoded to one of those words under the old JSON-string-quoting validation, is `mds::invalid_options` now.
- **`#422` (open, not yet fixed as of v0.5.0 Wave 1.5): three call sites propagate `env.from_js_value(...)?` directly** — `reject_unknown_napi_keys`'s property-name enumeration, `extract_vars_direct`'s `vars` conversion, and `extract_rules_direct`'s `rules` conversion. If the underlying JS-to-JSON conversion itself fails (e.g. a value napi/serde cannot represent, such as a `Symbol`), the resulting `napi::Error` propagates via `?` uncoded — it carries no `.code`/`.help`/`.span` the way every other options-validation error in this file does (those all go through `throw_options_error`/`throw_mds_error`). Don't assume every thrown error from this crate has a `.code` property until #422 lands.

## Key Files

- `crates/mds-napi/src/lib.rs` — the entire napi implementation; `build_canonical_result`, all seven `#[napi]` exports, error helpers, `extract_rules_direct`/`parse_rule_severities` call site
- `crates/mds-napi/Cargo.toml` — the `serde-json` napi feature is enabled at the workspace level, not here
- `crates/mds-napi/__test__/index.spec.mjs` — 126 JS tests; "intrinsic output shape" group (K-MD-*, K-MSG-*, K-MIXED-*, K-VARS-*); AC-API-05 deletion assertions; `F-CF8` (not-MDS path-typed-vs-canonical regression, #417); `L-N-RULES-ESC` (rules-error escaping, #418)

## Related

- Feature: mds-compiler — `mds::CompileResult`, `mds::CompiledOutput`, `mds::compile_with_deps`, `to_canonical_json`, `parse_rule_severities`, `Severity`/`FromStr` are the core inputs this crate delegates to
- Feature: mds-lint — the full lint JSON wire shape, rule/severity/fix-edit semantics, and `attach_lint_warnings`; this file covers only the napi FFI plumbing around `lint`/`lintFile`/`lintVirtual`
- Feature: mds-js — the JS package that re-exports these shapes as TypeScript types; `MarkdownResult`/`MessagesResult`/`LintResult` unions match the wire format exactly
- Feature: bundler-plugins — also consumes `compileFile` return shape via `MdsApi` interface
- PF-035 — local napi rebuild staleness trap (`--platform` vs the base `.node` filename the test harness loads)
- PF-014 — sanitize/escape at construction, not by post-processing a rendered message (the pattern `parse_rule_severities` and `format_unknown_keys_error` follow)
- PF-033 — a fix applied to one surface (a rules-error escaping fix) must be censused across every sibling surface (napi/WASM/Python) that serializes the same datum, not assumed fixed everywhere
