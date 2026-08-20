---
feature: mds-js
name: "@mdscript/mds universal JS package — option forwarding, backends, published TS types"
description: "Use when modifying the JS/TS public API surface, adding backend methods, changing option types, debugging basePath rejection behaviour, changing result types, updating the backend contract, working on WASM/native backend validation, or debugging why a backend result is rejected. Keywords: compileFile, compile, check, checkFile, lint, lintFile, lintVirtual, CompileResult, MarkdownResult, MessagesResult, CheckResult, LintResult, LintDiagnostic, LintFileOptions, CompileFileOptions, FileOptions, assertResultShape, validateBackendMethods, METHOD_KEYS, forwardOpts, assertKnownKeys, getBasePathError, BASEPATH_REJECTORS, BASE_METHODS, NODE_METHODS, WASM_EXPORTS, discriminated union, kind, mds::invalid_backend_result, mds::invalid_options, basePath, synchronous throw, native.ts, wasm.ts, contract.ts, types.ts, node.ts, browser.ts, options.ts."
category: component-patterns
directories: ["packages/mds/src", "packages/mds/__test__"]
referencedFiles:
  - packages/mds/src/types.ts
  - packages/mds/src/util/options.ts
  - packages/mds/src/backend/contract.ts
  - packages/mds/src/backend/native.ts
  - packages/mds/src/backend/wasm.ts
  - packages/mds/src/node.ts
  - packages/mds/src/browser.ts
  - packages/mds/__test__/options-validation.spec.mjs
  - packages/mds/__test__/types/consumer-node.ts
  - packages/mds/__test__/types/consumer-browser.ts
created: 2026-06-26
updated: 2026-08-21
---

# @mdscript/mds Universal JS Package

## Overview

`packages/mds/` is the universal JS/TS package. It wraps either the native NAPI addon (Node.js) or the WASM module (browser/WASM Node) behind a unified API. The seven public methods are `compile`, `check`, `compileFile`, `checkFile`, `lint`, `lintFile`, and `lintVirtual`. Option validation is handled in a single table-driven choke point (`util/options.ts`) before any backend call. `compile`/`compileFile` return a discriminated union (`CompileResult = MarkdownResult | MessagesResult`), not a flat string.

## Core Responsibilities

- Export all seven public methods and `init`, `getBackend`, `isMdsError` to consumers
- Validate options against per-method key lists (`METHOD_KEYS`) before backend dispatch
- Reject `basePath` on file-surface methods with purpose-built errors whose messages are byte-identical to napi
- Validate that the loaded backend exposes the required method names
- Validate each method call returns the correct result shape (shallow O(1) check)
- Re-export all public types from `node.ts` (file types) and `browser.ts` (string-surface types only)
- Does NOT implement compilation — delegates entirely to the backend

## Option Forwarding Architecture

All option validation and forwarding flows through `util/options.ts`. This is the single authoritative choke point; the per-surface builder functions (`varsOpt`/`compileOpt`/`lintOpt`/`lintFileOpt`) that used to hardcode their own key arrays are deleted. Any reference to those builders is stale.

**`METHOD_KEYS`** — a table mapping each `MethodName` to its allowed option keys, derived at build time from the public interfaces via `keysOf<T>`. Adding a key to an option interface requires updating the `keysOf<T>` witness literal or the call becomes a compile error.

**`forwardOpts(options, method)`** — picks only `METHOD_KEYS[method]` keys from `options`, filtering out `null`/`undefined` values. Returns `undefined` when all accepted keys are absent (preserves the backend no-options fast path). `basePath` is absent from `METHOD_KEYS` for `compileFile`, `checkFile`, `lintFile`, and `lintVirtual` — those surfaces have a dedicated rejection path.

**`assertKnownKeys(options, method)`** — throws `mds::invalid_options` synchronously if `options` contains any key not in `METHOD_KEYS[method]`. Message format matches `format_unknown_keys_error` in `mds-core/src/options.rs` byte-for-byte (enforced by U-OV-14 / U-OV-31 `strictEqual` against the live napi message). Key ORDER in `METHOD_KEYS` entries is load-bearing because it determines the `recognised keys are: …` list — do not reorder without verifying the napi order matches.

**`BASEPATH_REJECTORS`** — a `ReadonlyMap<MethodName, () => Error>` covering `compileFile`, `checkFile`, `lintFile`, `lintVirtual`. Each entry maps to a purpose-built error factory with a message byte-identical to the corresponding napi parser (`parse_file_opts`, `parse_check_file_opts`, `parse_lint_file_opts`, `parse_lint_virtual_opts` in `crates/mds-napi/src/lib.rs`). Adding a new file-surface method requires adding it to this map — the Map literal makes that a TypeScript error, not a silent omission.

**`getBasePathError(options, method)`** — checks `options.basePath !== undefined` (including explicit `null`) and returns the factory's error when the method is in `BASEPATH_REJECTORS`. The public wrapper calls this AFTER `assertKnownKeys` and throws synchronously.

The key invariant worth preserving: **any new option added to one surface method must reject on the same criterion across all four file-surface methods, and any purpose-built rejection message must be byte-identical to napi.** U-OV-27 (`strictEqual` wrapper vs napi vs WASM subprocess) enforces this at runtime.

Option key lists are hardcoded independently in **four** places: TS `METHOD_KEYS`, napi, wasm, Python decorators. TS↔napi is runtime-bound by U-OV-14/U-OV-31; WASM and Python rest on prose alone. Centralizing this is tracked in issue **#311**.

## basePath Surface Matrix

| Method | basePath valid? | Rejection mechanism |
|---|---|---|
| `compile` | Yes (native) / throws on WASM | WASM: `throwWasmBasePathError()`; native: forwarded |
| `check` | Yes (native) / throws on WASM | Same as compile |
| `lint` | Yes (native) / throws on WASM | Same as compile |
| `compileFile` | Never | `BASEPATH_REJECTORS` — purpose-built error, synchronous |
| `checkFile` | Never | Same |
| `lintFile` | Never | Same |
| `lintVirtual` | Never | Same |

The regression that triggered F-03: `lintFile`/`lintVirtual` used to reject on **key presence** (`assertKnownKeys`) while `compileFile`/`checkFile` rejected on **value** (`BASEPATH_REJECTORS`). Because `exactOptionalPropertyTypes` is absent repo-wide, `{basePath: undefined}` typed against `basePath?: never` would type-check but then throw at runtime. Fixed by adding `lintFile`/`lintVirtual` to `BASEPATH_REJECTORS` with their own purpose-built factories. The guard also fires for explicit `null` (matching napi's `has_named_property` behaviour).

`{basePath: undefined}` is treated as "key absent" — the `getBasePathError` check uses `!== undefined`, and `forwardOpts` drops `null`/`undefined` values, so napi's `has_named_property` gate never fires for it. Both backends agree (enforced by U-OV-29).

## Published Types

### CompileResult — discriminated union

Branch on `result.kind` to narrow. `output` exists only on `MarkdownResult`; `messages` only on `MessagesResult`.

```typescript
// The inactive field is ABSENT, not null — assertResultShape rejects
// { kind: 'markdown', output: '...', messages: [] } (inactive field present)
export type CompileResult = MarkdownResult | MessagesResult;
```

### File-surface type naming family

`CompileFileOptions` is the canonical interface. `FileOptions` is a `@deprecated` alias kept for backward compatibility. The naming family is now `CompileFileOptions`, `CheckFileOptions`, `LintFileOptions`.

`CompileFileOptions` deliberately does NOT extend `CompileOptions` — after `CompileOptions` gained `basePath`, inheritance would silently add an invalid field to the file surface. All file-surface option fields are declared directly.

`MdsBackend` (previously declared once, exported from neither entry point) is deleted. Do not re-introduce it.

### LintDiagnostic nullability

`LintDiagnostic.help` is `string | null | undefined` (declared as `?: string | null`), and `LintDiagnostic.span` is `LintSpan | null | undefined` (declared as `?: LintSpan | null`). The `?` (undefined) was deliberately kept: with `span?: LintSpan | null`, `diag.span !== undefined` narrows to `LintSpan | null` under `strict`. Dropping `?` would be a further source-breaking narrowing. These shapes are pinned by `consumer-node.ts` / `consumer-browser.ts` type fixtures (testing-03).

### Export divergence: node.ts vs browser.ts

File-surface types (`CompileFileOptions`, `CheckFileOptions`, `FileOptions`, `InitOptions`) are exported from `node.ts` only — `browser.ts` has no file operations. This divergence is intentional. `LintFileOptions` IS exported from `browser.ts` (used by `lintVirtual`).

String-surface types (`CompileOptions`, `CheckOptions`, `LintOptions`) are shared between both entries and carry `basePath` on both (ADR-011). The WASM backend enforces the constraint at runtime with `mds::invalid_options`; no narrowed browser-only alias exists.

## Backend Layers

### NapiAddon interface (native.ts)

Now covers all seven method names including `lint`, `lintFile`, `lintVirtual`. The lint file surface uses `NapiLintFileOpts = { vars?, rules? }` — no `basePath`. The lint string surface uses `NapiLintOpts = { basePath?, vars?, rules? }`.

### WasmModule interface (wasm.ts)

Exports `compile`, `check`, `lint`, `lintVirtual`, `scanImports`. `lintFile` is NOT in `WasmModule` — file operations are added via `wrapWithFileOps` in `node.ts`.

`lintVirtual` on the WASM backend deliberately OMITS the `basePath` guard — `LintFileOptions.basePath` is `never` and the public wrapper's `BASEPATH_REJECTORS` rejects it first. That omission applies ADR-011 and should NOT be "fixed".

### Defense in depth across backends

`createNativeBackend` and `createWasmBackend`'s `wrapWithFileOps` each carry per-method `basePath` guards (citing `avoids PF-004`) in addition to the public wrapper's, covering `compileFile`, `checkFile`, `lintFile`, and `lintVirtual`. An internal caller that obtains a backend directly and bypasses the public wrapper's `BASEPATH_REJECTORS` is caught here. Without these backend-level guards, `forwardOpts` would silently drop `basePath` on native (since it's absent from `METHOD_KEYS` for those surfaces) while WASM would throw — producing asymmetric behavior on the same call.

## Synchronous Throws Contract

`compileFile`, `checkFile`, and `lintFile` are non-`async` functions that return Promises. All option-validation errors (unknown keys AND basePath) throw **synchronously**, before any I/O. Callers using `try { compileFile(f, opts) } catch` capture both error classes synchronously. `.catch()` on the returned promise does NOT receive option-validation errors.

Tests for this must use `assert.throws` (not `assert.rejects`) — a regression to async throw would escape a `rejects` validator and surface as a test failure with misleading `testCodeFailure`, masking the regression (see U-OV-32/33).

`lintVirtual` is synchronous (no I/O), so this distinction does not apply to it.

## Method Manifests in contract.ts

```typescript
export const BASE_METHODS = ['compile', 'check'] as const;
export const NODE_METHODS = ['compileFile', 'checkFile'] as const;
// NODE_METHODS does NOT include lint methods — those are wired directly in native.ts
export const WASM_EXPORTS = [...BASE_METHODS, 'scanImports'] as const;
// WASM_EXPORTS does NOT include lint — lintFile is WASM-implemented via wrapWithFileOps
```

`createNativeBackend` validates `[...BASE_METHODS, ...NODE_METHODS]` plus `lint`, `lintFile`, `lintVirtual`. Missing method throws a plain `Error` (not `mds::` coded) naming the missing method.

## assertResultShape — O(1) shallow validator

- `kind='compile'`: branches on `result.kind`; asserts `output` is string or `messages` is array; asserts the **inactive field is absent**; asserts `warnings`/`dependencies` are arrays
- `kind='check'`: only asserts `warnings` is array
- `kind='lint'`: asserts `files` is array, `truncated` is boolean, `version` is number
- PERF-04 constraint: uses only `Array.isArray()` — never accesses array elements. A Proxy must observe zero numeric-index reads.
- Throws `Error` with `code: 'mds::invalid_backend_result'` on shape violation

## Anti-Patterns

- **Re-introducing per-surface builder functions** — `varsOpt`/`compileOpt`/`lintOpt`/`lintFileOpt` were deleted because they each hardcoded their own key arrays, which drifted from `METHOD_KEYS` without a compile error. This was the root shape of PF-004 / #180.
- **Hardcoding basePath rejection in only some of the four file-surface methods** — the F-03 regression. All four must use `BASEPATH_REJECTORS` with purpose-built errors and identical napi messages.
- **Writing a basePath rejection test with `.catch()` or `assert.rejects`** — synchronous throws escape both; must use `assert.throws`.
- **Accessing `result.output` without branching on `result.kind`** — TypeScript will catch this at compile time since `CompileResult` is a union.
- **Including `compileMessages` in `WASM_EXPORTS` or `BASE_METHODS`** — deleted from WASM binding.
- **Returning `{ kind: 'markdown', output: ..., messages: [] }` from mocks** — inactive field must be ABSENT; `assertResultShape` rejects it.
- **Adding a basePath guard to WASM `lintVirtual`** — the public wrapper fires first; the guard in `lintVirtual` is deliberately absent (ADR-011).
- **Reordering METHOD_KEYS witness literals** — key order determines the `recognised keys are: …` list; U-OV-14/U-OV-31 pins it via `strictEqual` against live napi, not a hardcoded string.

## Gotchas

- **`lintFile`/`lintVirtual` on the file surface throw for basePath — use `assert.throws`, not `assert.rejects`** — this is the same synchronous-throw channel as `compileFile`/`checkFile`.
- **Type fixtures import from `../../dist/node.js`** — `consumer-node.ts` and `consumer-browser.ts` are compiled by `tsc -p tsconfig.types.json` and import from `dist/`. A stale or missing `dist/` silently typechecks yesterday's declarations. Build before running type tests.
- **Inferred-object shape `{ basePath: '/' }` IS rejected by file-surface types** — despite what early PR descriptions claimed, `TS2322` fires: `string` is not assignable to `undefined` (the effective type of `basePath?: never`). Consumer-node.ts lines 95-97 encode this.
- **Cross-surface differential tests hard-fail under `process.env.CI` when CLI/WASM is missing, but warn-and-return locally** — a green local run is NOT evidence; only CI is.
- **`CheckResult` has only `{ warnings: string[] }` — no `dependencies`** — this matches the napi wire; check does not expose deps.
- **The `assertReady()` error message is byte-literal** — `'@mdscript/mds: call await init() before using compile/check/compileFile/checkFile/getBackend'` — tests pin it; don't change without updating tests.
- **`basePath: undefined` is treated as "key absent"** — `getBasePathError` uses `!== undefined`, not `!= null`. Both backends must agree (enforced by U-OV-29). This is consistent with `forwardOpts`'s `!= null` filter that prevents `{basePath: undefined}` from reaching napi.
- **Option key lists live in four independent places** (TS `METHOD_KEYS`, napi, wasm, Python). TS↔napi parity is runtime-enforced; WASM and Python drift is caught only by prose review until #311 lands.

## Key Files

- `packages/mds/src/types.ts` — all public types: `Message`, `MarkdownResult`, `MessagesResult`, `CompileResult`, `CheckResult`, `CompileOptions`, `CheckOptions`, `CompileFileOptions` (canonical), `FileOptions` (deprecated alias), `CheckFileOptions`, `LintOptions`, `LintFileOptions`, `LintResult`, `LintDiagnostic`, `LintSpan`, `MdsBaseBackend`, `MdsNodeBackend`
- `packages/mds/src/util/options.ts` — `METHOD_KEYS`, `forwardOpts`, `assertKnownKeys`, `getBasePathError`, `BASEPATH_REJECTORS`, `MethodName`
- `packages/mds/src/backend/contract.ts` — `BASE_METHODS`, `NODE_METHODS`, `WASM_EXPORTS`, `ResultKind`, `assertResultShape`, `validateBackendMethods`
- `packages/mds/src/backend/native.ts` — `NapiAddon` interface, `createNativeBackend` (with per-method depth-in-defense basePath guards)
- `packages/mds/src/backend/wasm.ts` — `WasmModule` interface, `createWasmBackend`, `fileOpts`, WASM basePath error
- `packages/mds/src/node.ts` — `wrapWithFileOps`, all seven public functions, type re-exports
- `packages/mds/src/browser.ts` — browser-safe re-exports; no file operations
- `packages/mds/__test__/options-validation.spec.mjs` — U-OV-1..U-OV-36; option validation, basePath forwarding, synchronous-throw contract, byte-identical message parity
- `packages/mds/__test__/types/consumer-node.ts` — AC-P3-20/21 type-level matrix for Node entry
- `packages/mds/__test__/types/consumer-browser.ts` — AC-P3-16/20 type-level matrix for browser entry

## Related

- ADR-011 — String-surface option types are shared between Node.js and browser entries; `basePath` enforcement on WASM is runtime-only, not type-level. AC-P3-20 requires `basePath` to be a POSITIVE case in `consumer-browser.ts`.
- PF-004 (avoids) — Alternate code path silently bypassing an enforcement point. The per-backend depth-in-defense basePath guards, and the unified `forwardOpts` replacing per-surface builders, both apply this lesson.
- PF-013 (avoids) — Vacuous absence-only assertions. U-OV-22 (wrong basePath throws, right basePath succeeds) and U-OV-14/U-OV-31 (`strictEqual` against live napi, not hardcoded string) apply this lesson. The `assertWrapperAcceptsBasePath` helper checks WHICH error was thrown, not merely that something threw.
- Feature: mds-napi — the native backend; compile/compileFile return the discriminated union this package types
- Feature: mds-lint — the lint surface; `LintResult`, `LintDiagnostic`, `LintFileOptions` types here are the wire contract for mds-napi's lint output
- Feature: bundler-plugins — imports `compileFile` from this package via `MdsApi` in `bundler-utils/src/types.ts`
- Feature: mds-compiler — the Rust `CompileResult`/`CompiledOutput` types that drive the wire format
