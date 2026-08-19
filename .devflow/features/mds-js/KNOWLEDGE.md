---
feature: mds-js
name: MDS JavaScript Package (@mdscript/mds)
description: "Use when modifying the JS/TS public API surface, adding backend methods, changing result types, updating the backend contract, working on WASM/native backend validation, or debugging why a backend result is rejected. Keywords: compileFile, compile, check, checkFile, CompileResult, MarkdownResult, MessagesResult, CheckResult, CompileMessagesResult, assertResultShape, validateBackendMethods, BASE_METHODS, NODE_METHODS, WASM_EXPORTS, discriminated union, kind, mds::invalid_backend_result, native.ts, wasm.ts, contract.ts, types.ts, node.ts, browser.ts."
category: component-patterns
directories: ["packages/mds/"]
referencedFiles:
  - packages/mds/src/types.ts
  - packages/mds/src/backend/contract.ts
  - packages/mds/src/backend/native.ts
  - packages/mds/src/backend/wasm.ts
  - packages/mds/src/node.ts
  - packages/mds/src/browser.ts
  - packages/mds/src/index.ts
  - packages/mds/__test__/intrinsic-output.spec.mjs
  - packages/mds/__test__/backend-contract.spec.mjs
created: 2026-06-26
updated: 2026-06-26
---

# MDS JavaScript Package (@mdscript/mds)

## Overview

`packages/mds/` is the universal JS/TS package. It wraps either the native NAPI addon (Node.js) or the WASM module (browser/WASM Node) behind a unified API. After the intrinsic-output refactor, `compile` and `compileFile` return a **discriminated union** (`CompileResult = MarkdownResult | MessagesResult`), not a flat string. The `compileMessages`/`compileMessagesFile`/`CompileMessagesResult` symbols are deleted.

## Core Responsibilities

- Export `compile`, `compileFile`, `check`, `checkFile`, `init`, `getBackend`, `isMdsError` to consumers
- Validate that the loaded backend (native addon or WASM module) exposes the required method names
- Validate that each compile/check call returns the correct result shape (shallow O(1) check)
- Re-export all public types (`CompileResult`, `MarkdownResult`, `MessagesResult`, `CheckResult`, `Message`, etc.)
- Does NOT implement compilation — delegates entirely to the backend

## Standard Structure

### CompileResult — the discriminated union type

```typescript
// packages/mds/src/types.ts
export interface MarkdownResult {
  kind: 'markdown';
  output: string;         // rendered Markdown — present only on markdown results
  warnings: string[];
  dependencies: string[];
}

export interface MessagesResult {
  kind: 'messages';
  messages: Message[];    // array of {role, content} — present only on messages results
  warnings: string[];
  dependencies: string[];
}

export type CompileResult = MarkdownResult | MessagesResult;

export interface CheckResult {
  warnings: string[];
  // NOTE: no dependencies — check does not expose deps
}
```

Branch on `result.kind` to narrow to the specific variant:

```typescript
if (result.kind === 'markdown') {
  // result.output is string here
} else {
  // result.messages is Message[] here
}
```

### Method manifests in contract.ts

```typescript
// contract.ts
export const BASE_METHODS = ['compile', 'check'] as const;
export const NODE_METHODS = ['compileFile', 'checkFile'] as const;
export const WASM_EXPORTS = [...BASE_METHODS, 'scanImports'] as const;
// WASM_EXPORTS does NOT include compileMessages — it was deleted from the wasm binding
```

`createNativeBackend` validates `[...BASE_METHODS, ...NODE_METHODS]`; `createWasmBackend` validates `WASM_EXPORTS`. Any missing method throws a plain `Error` (not an `mds::` coded error) naming the missing method.

### assertResultShape — O(1) shallow validator

```typescript
export function assertResultShape(result: unknown, kind: ResultKind): void
```

- `kind='compile'`: branches on `result.kind`; checks `output` is string (markdown) or `messages` is array (messages); asserts the **inactive field is absent** (`'messages' in r` for markdown, `'output' in r` for messages); asserts `warnings` and `dependencies` are arrays
- `kind='check'`: only asserts `warnings` is array
- Throws `Error` with `code: 'mds::invalid_backend_result'` on shape violation
- PERF-04 constraint: uses only `Array.isArray()` — never accesses array elements or indexes. A Proxy must observe zero numeric-index reads.
- Extra fields on valid results are silently tolerated

### Deleted exports

These symbols are removed and must not be re-exported anywhere:
- `compileMessages` (from `node.ts`, `browser.ts`, `index.ts`)
- `compileMessagesFile` (from `node.ts`, `index.ts`)
- `CompileMessagesResult` type (from `types.ts`, `index.ts`)

The test files `compile-messages.spec.mjs` and `wasm-compileMessages.spec.mjs` are also deleted. The new test `intrinsic-output.spec.mjs` (U-IO-1..U-IO-22) asserts that the deleted exports are absent:

```javascript
// U-IO-21/22 — these must pass
assert.strictEqual(typeof module.compileMessages, 'undefined');
assert.strictEqual(typeof module.compileMessagesFile, 'undefined');
```

### NapiAddon interface (native.ts)

```typescript
interface NapiAddon {
  compile(source: string, opts?: unknown): unknown;
  check(source: string, opts?: unknown): unknown;
  compileFile(path: string, opts?: unknown): unknown;  // async via JS wrapper
  checkFile(path: string, opts?: unknown): unknown;
  // NO compileMessages, NO compileMessagesFile
}
```

All methods return `unknown` (the actual value is `serde_json::Value` from PR-2). `assertResultShape` validates the shape before returning to the caller.

### WasmModule interface (wasm.ts)

```typescript
interface WasmModule {
  compile(source: string, opts?: unknown): unknown;
  check(source: string, opts?: unknown): unknown;
  scanImports(source: string): unknown;
  default?: () => Promise<void>; // optional initializer
  // NO compileMessages
}
```

### ResultKind type

```typescript
export type ResultKind = 'compile' | 'check';
```

Used as the discriminant for `assertResultShape`. The `'compile'` case covers both `MarkdownResult` and `MessagesResult` — the function branches internally on `result.kind`.

## Dependency Patterns

Mock MDS APIs in tests must return the new discriminated-union shape:

```javascript
// Correct mock shape (not the old flat { output: ... })
{ kind: 'markdown', output: '...', warnings: [], dependencies: [] }
// or
{ kind: 'messages', messages: [{role:'user', content:'hi'}], warnings: [], dependencies: [] }
```

## Error Handling

`compile`/`compileFile` propagate `mds::mixed_content` from the backend when the source file has mixed content. The JS package does not catch or transform these errors — they pass through to the caller.

`assertResultShape` errors have `code: 'mds::invalid_backend_result'` — distinguishable from compiler errors (`mds::*` from `mds-core`) so callers can tell "shape problem" from "compile error".

## Anti-Patterns

- **Accessing `result.output` without branching on `result.kind`** — TypeScript will catch this at compile time since `CompileResult` is a union; `output` is only on `MarkdownResult`
- **Re-exporting `compileMessages` or `CompileMessagesResult`** — deleted; test spec has negative assertions
- **Calling `Array.isArray()` then accessing elements in assertResultShape** — PERF-04 violation; the validator must be O(1); never access `[0]` or iterate
- **Including `compileMessages` in `WASM_EXPORTS` or `BASE_METHODS`** — deleted from WASM binding; adding it here will cause validateBackendMethods to throw on valid WASM modules
- **Returning `{ kind: 'markdown', output: ..., messages: [] }` from mocks** — inactive field must be absent; assertResultShape rejects it

## Gotchas

- `CheckResult` has only `{ warnings: string[] }` — no `dependencies`. This matches the napi wire (check returns warnings only). Not a regression — pre-change CheckResult was the same shape.
- `WASM_EXPORTS` includes `scanImports` (needed for JS-side file resolution) but `BASE_METHODS` does not. Don't copy `WASM_EXPORTS` content into `BASE_METHODS`.
- The `assertReady()` error message is "Call await init() before using compile" — this exact string appears in tests; don't change it without updating the tests.
- Mock MDS APIs in older tests may still return the old `{ output: string, warnings, deps }` shape. Update them to `{ kind: 'markdown', output: ..., warnings: [], dependencies: [] }`.

## Key Files

- `packages/mds/src/types.ts` — `Message`, `MarkdownResult`, `MessagesResult`, `CompileResult`, `CheckResult`, `MdsBaseBackend`, `MdsNodeBackend`
- `packages/mds/src/backend/contract.ts` — `BASE_METHODS`, `NODE_METHODS`, `WASM_EXPORTS`, `ResultKind`, `assertResultShape`, `validateBackendMethods`
- `packages/mds/src/backend/native.ts` — `NapiAddon` interface, `createNativeBackend`
- `packages/mds/src/backend/wasm.ts` — `WasmModule` interface, `createWasmBackend`
- `packages/mds/src/node.ts` — `wrapWithFileOps`, public `compileFile`/`checkFile`; exports `MarkdownResult`, `MessagesResult`
- `packages/mds/src/browser.ts` — browser-safe re-exports; exports `Message`, `MarkdownResult`, `MessagesResult`
- `packages/mds/__test__/intrinsic-output.spec.mjs` — U-IO-1..U-IO-22; tests for kind branching + absence assertions
- `packages/mds/__test__/backend-contract.spec.mjs` — U-BC1..U-BC20; assertResultShape shape + absence tests

## Related

- Feature: mds-napi — the native backend; `compile`/`compileFile` return the discriminated union this package types
- Feature: bundler-plugins — imports `compileFile` from this package via the `MdsApi` interface in `bundler-utils/src/types.ts`
- Feature: mds-compiler — the Rust `CompileResult`/`CompiledOutput` types that drive the wire format
