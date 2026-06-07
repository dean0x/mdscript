---
feature: mds-js
name: MDS JavaScript Package (@mdscript/mds)
description: "Use when working on the @mdscript/mds JavaScript package. Keywords: MdsBaseBackend, MdsNodeBackend, MdsBackend, initWasmNode, initWasmBrowser, createWasmBackend, createNativeBackend, browser entry, node entry, module-scanner, buildModulesMap, findProjectRoot, normalizeVirtualKey, wrapWithFileOps, O_NOFOLLOW, TOCTOU, WasmModule, validateWasmShape, init, compileFile, checkFile, compileMessages, compile_messages, Message, CompileMessagesResult, @message, @end, structured chat messages, circuit breaker, promise dedup, MDS_BACKEND, forceBackend, ensureBackend, LazyInit, loadNativeBackend, loadWasmNodeBackend, varsOpt, fileOpts, compileOpts, cross-directory imports, project root discovery, .git marker, .mdsroot marker, entryFilename, relative path key, DEFAULT_MAX_MODULES, DEFAULT_MAX_AGGREGATE_SIZE, MAX_IMPORT_DEPTH, MAX_TRAVERSAL_DEPTH."
category: component-patterns
directories: [packages/mds/]
referencedFiles:
  - packages/mds/src/types.ts
  - packages/mds/src/index.ts
  - packages/mds/src/node.ts
  - packages/mds/src/browser.ts
  - packages/mds/src/backend/wasm.ts
  - packages/mds/src/backend/native.ts
  - packages/mds/src/util/module-scanner.ts
  - packages/mds/src/util/options.ts
  - packages/mds/package.json
  - packages/mds/__test__/compile-messages.spec.mjs
created: 2026-05-27
updated: 2026-06-07
---

# MDS JavaScript Package (@mdscript/mds)

## Overview

`@mdscript/mds` is the universal JavaScript package for the MDS compiler (version 0.2.0). It provides two entry points:
- **`dist/node.js`** — Node.js environments; tries the native (`@mdscript/mds-napi`) backend first, falls back to WASM (`@mdscript/mds-wasm`)
- **`dist/browser.js`** — browser/edge environments; WASM-only, no file operations

The package conditionally selects the backend using Node.js `package.json` exports conditions (`"node"` / `"default"`).

**Repository**: `https://github.com/dean0x/mdscript` (renamed from `dean0x/mds` at v0.2.0).

## Type Hierarchy

All types are defined in `packages/mds/src/types.ts`:

| Type | Description |
|---|---|
| `MdsBaseBackend` | Browser-safe interface: `compile`, `check`, `compileMessages`, `getBackend` |
| `MdsNodeBackend` | Extends `MdsBaseBackend` with `compileFile`, `checkFile` |
| `MdsBackend` | Deprecated alias for `MdsNodeBackend` |
| `CompileResult` | `{ output: string; warnings: string[]; dependencies: string[] }` |
| `CheckResult` | `{ warnings: string[] }` |
| `Message` | `{ role: string; content: string }` — one structured chat message |
| `CompileMessagesResult` | `{ messages: Message[]; warnings: string[]; dependencies: string[] }` |
| `CompileOptions` | `{ vars?: Record<string, unknown> }` |
| `FileOptions` | `{ vars?: Record<string, unknown> }` |
| `InitOptions` | `{ wasmUrl?: string \| URL \| Response \| BufferSource }` |
| `BackendType` | `'native' \| 'wasm'` |
| `MdsError` | Extends `Error` with `code: string`, `help?: string`, `span?: MdsErrorSpan` |
| `MdsErrorSpan` | `{ offset, length, line?, column? }` — byte-based, 1-indexed line/column |
| `WasmModule` | WASM module shape: `compile`, `check`, `compileMessages`, `scanImports`, optional `default` |

`MdsBaseBackend` now includes `compileMessages` — it is browser-safe (no file I/O) and lives on the base interface alongside `compile` and `check`.

**`isMdsError(err: unknown): err is MdsError`** — type guard. Returns `true` when `err` is an `Error` with a `.code` string starting with `'mds::'`.

## compileMessages() Entry Point

`compileMessages(source: string, options?: CompileOptions): CompileMessagesResult` compiles `@message` blocks in an MDS source string into a structured array of chat messages. Exported from both `dist/node.js` and `dist/browser.js`.

**Result shape**:
- `messages: Message[]` — ordered array of `{ role, content }` objects extracted from `@message <role>:` ... `@end` blocks
- `warnings: string[]` — non-fatal diagnostics (e.g. orphan text outside a `@message` block)
- `dependencies: string[]` — absolute paths of all transitively imported files

**Key behaviors** (from U-CM1–U-CM12):
- Bare-word roles (e.g. `@message system:`) are string literals — never treated as variable lookups even when a var with the same name exists
- Dynamic roles via `{r}` interpolation in the role position (e.g. `@message {r}:`) resolve from `vars`
- Messages with empty body after trimming are omitted from the output
- Orphan text outside `@message` blocks produces a warning (not an error)
- A source with no `@message` blocks at all throws `MdsError` (code `mds::...`)
- Nested `@message` inside another `@message` throws `MdsError`
- `@for` loops that expand to multiple `@message` blocks produce multiple `Message` entries

**compileMessages is synchronous** — like `compile` and `check`, it delegates directly to the backend without file I/O. Requires `await init()` first.

## Backend-Adapter Wiring for compileMessages

`compileMessages` follows the exact same layered adapter pattern as `compile` and `check`. All four layers must be updated together:

1. **`types.ts`**: `MdsBaseBackend.compileMessages` declared (browser-safe, no file ops)
2. **`backend/native.ts`**: `NapiAddon.compileMessages` added to the interface; `createNativeBackend` adapter delegates `addon.compileMessages(source, varsOpt(options))`
3. **`backend/wasm.ts`**: `WasmModule.compileMessages` added to the interface; `createWasmBackend` adapter delegates `wasmModule.compileMessages(source, compileOpts(options))`
4. **`node.ts` / `browser.ts`**: `compileMessages` exported as a top-level function that calls `assertReady().compileMessages(source, options)`

The `compileMessages` adapter in `wasm.ts` uses `compileOpts()` (same as `compile`/`check`) — not `fileOpts()`. There is no file-based `compileMessagesFile` variant.

## validateWasmShape — compileMessages Is Now Required

`validateWasmShape(mod: unknown): asserts mod is WasmModule` checks four exports: `compile`, `check`, `compileMessages`, and `scanImports`. A WASM module missing any of these four throws immediately with a descriptive error naming the missing member.

**Consequence**: any WASM module built before `compileMessages` was added to `mds-wasm` will be rejected at init time with an actionable error. Rebuilding the WASM package with `wasm-pack build crates/mds-wasm` resolves it.

Test U-WB21 exercises this path — a module `{ compile, check, scanImports }` (no `compileMessages`) must cause `validateWasmShape` to throw mentioning `"compileMessages"`.

## Node.js Entry (`packages/mds/src/node.ts`)

### Backend Selection

`MDS_BACKEND` env var controls backend selection:
- `'native'` — force native (`@mdscript/mds-napi`) backend; error if unavailable
- `'wasm'` — force WASM backend; skip native probe
- Any other value — warning emitted, treated as unset
- Unset (default) — try native first, fall back to WASM

`ensureBackend(options?)` is the single source of truth for backend initialization. It deduplicates concurrent `init()` calls by caching the in-flight `Promise<void>`. On rejection, `initPromise` is cleared so the next `init()` call retries.

### init() Contract

`init(options?: InitOptions): Promise<void>` — must be called and awaited before any other function. Idempotent: subsequent calls resolve immediately once the backend is set. Concurrent calls share one promise (no double-initialization race).

### File Operations

`wrapWithFileOps(base: MdsBaseBackend, wasmModule: WasmModule): MdsNodeBackend` — wraps a browser-safe backend with `compileFile`/`checkFile` that:
1. Call `buildModulesMap(path, wasmModule.scanImports)` to resolve all transitive imports into a flat `Record<string, string>`.
2. Extract the entry source from the modules map (keyed by `entryFilename`).
3. Delete `modules[entryFilename]` before calling WASM to prevent `mds::filename_collision`.
4. Call `wasm.compile({ filename: entryFilename, modules: remainingModules, vars? })` or `wasm.check(...)`.

The native backend's `compileFile`/`checkFile` are synchronous wrappers directly over the napi addon — no modules map needed.

### Backend Loaders

- `loadNativeBackend()` — dynamic `require('@mdscript/mds-napi')` wrapped in try/catch; returns `{ backend, error: null }` on success or `{ backend: null, error }` on failure. Never throws.
- `loadWasmNodeBackend(options?)` — calls `initWasmNode(options)` then `createWasmBackend(module)` then `wrapWithFileOps`. Always returns a `MdsNodeBackend`. Throws if WASM module cannot be loaded.

### Test Utilities

- `_resetForTesting()` — clears `backend` and `initPromise`. FOR TESTING ONLY.

## Browser Entry (`packages/mds/src/browser.ts`)

Browser entry exposes only `compile`, `check`, `compileMessages`, `getBackend`, and `init`. No file operations.

`init(options?: InitOptions)` calls `initWasmBrowser(options)`, caches in `resolvedBackend`. Uses a separate `initVoidPromise` for concurrent-call deduplication. On rejection, `initVoidPromise` is reset to `null` (cleared) so the next call retries; `resolvedBackend` is never cleared once set.

### Test Utilities (browser)

- `_resetForTesting()` — clears both `resolvedBackend` and `initVoidPromise`.
- `_initWithModuleForTesting(mod: WasmModule)` — injects a pre-loaded module, bypassing `initWasmBrowser()`. Allows Node.js test suites to exercise the browser API surface.

## WASM Backend (`packages/mds/src/backend/wasm.ts`)

### WasmModule Shape

```typescript
interface WasmModule {
  compile(source: string, options?: { filename?: string; modules?: Record<string, string>; vars?: Record<string, unknown> }): CompileResult;
  check(source: string, options?: { filename?: string; modules?: Record<string, string>; vars?: Record<string, unknown> }): CheckResult;
  // Added in Issue #56 — all four exports are now required by validateWasmShape
  compileMessages(source: string, options?: { filename?: string; modules?: Record<string, string>; vars?: Record<string, unknown> }): CompileMessagesResult;
  scanImports(source: string): string[];
  default?: (input?: unknown) => Promise<void>;
}
```

All four functions (`compile`, `check`, `compileMessages`, `scanImports`) are required — `validateWasmShape` enforces this at init.

### Circuit Breaker Pattern

Both `initWasmNode` and `initWasmBrowser` implement a circuit breaker:
- `MAX_INIT_RETRIES = 3` (Node.js) / `MAX_BROWSER_RETRIES = 3` (browser)
- On failure: increment failure counter, clear cached promise (so next call retries)
- After exhaustion: every subsequent call throws immediately without re-attempting

`nodeFailures` and `browserFailures` are module-level counters. `_resetForTesting(failures?, browserFailuresCount?)` pre-seeds them for exhaustion path testing.

### Node.js WASM Initialization

`initWasmNode(options?)` deduplicates via `cachedNodePromise`. On first call:
1. Defers `require('node:module')` import to the async function body (browser-safe).
2. Calls `_initNode(options)` which iterates candidate paths via `tryLoadCandidate`.
3. On success, validates shape via `validateWasmShape`.

`tryLoadCandidate(candidate, require, wasmUrl)`:
- Returns `null` for `MODULE_NOT_FOUND` errors.
- Throws for shape validation failures or unexpected errors.
- Re-throws non-not-found errors so the caller can surface them.

`isModuleNotFound(err)` — detects `MODULE_NOT_FOUND` / `ERR_MODULE_NOT_FOUND` error codes.

`validateWasmShape(mod: unknown): asserts mod is WasmModule` — exported; checks `compile`, `check`, `compileMessages`, and `scanImports` are all present as functions. Throws a descriptive error naming the first missing member.

### Browser WASM Initialization

`initWasmBrowser(options?)` — no candidate list; calls `_initBrowser(options)` which dynamically imports the WASM module and calls its `default` initializer with `wasmUrl`. Simpler than Node.js — exhaustion means the `wasmUrl` itself is wrong.

### Options Builders

- `compileOpts(options?)` — merges `filename: 'input.mds'`, frozen empty `modules`, and optional `vars`. Returns a frozen object to prevent WASM FFI mutation of shared state. Used by `compile`, `check`, AND `compileMessages`.
- `fileOpts(entryFilename, modules, options?)` — for file operations; uses the real `entryFilename` and the resolved `modules` map, plus optional `vars`. Exported for use in `node.ts`.
- `DEFAULT_COMPILE_OPTS` — deep-frozen default object for the no-vars path; both outer object and `modules` are frozen.

### Factory

`createWasmBackend(wasmModule: WasmModule): MdsBaseBackend` — synchronous factory; mirrors `createNativeBackend(addon)` pattern. Returns `compile`, `check`, `compileMessages`, `getBackend` (always `'wasm'`) without file operations. File operations are added by `wrapWithFileOps` in `node.ts`.

### Test Utilities (wasm)

`_resetForTesting(failures?, browserFailuresCount?)` — full reset including both counter pre-seeding slots. Exported.

## Native Backend (`packages/mds/src/backend/native.ts`)

`createNativeBackend(addon: NapiAddon): MdsNodeBackend` — synchronous factory. The addon is injected (not imported directly) for testability. Returns `compile`, `check`, `compileMessages`, `compileFile` (sync), `checkFile` (sync), `getBackend` (always `'native'`).

`NapiAddon` interface documents the napi surface:
- `compile(source, opts?)` / `check(source, opts?)` / `compileMessages(source, opts?)` — accept `{ basePath?, vars? }`
- `compileFile(path, opts?)` / `checkFile(path, opts?)` — accept `{ vars? }` only

`varsOpt(options?)` from `../util/options.ts` builds `{ vars }` only when `options.vars` is defined and non-null. Used by `compile`, `check`, and `compileMessages` adapters in both native and WASM backends.

## Module Scanner (`packages/mds/src/util/module-scanner.ts`)

### Project Root Discovery

**`findProjectRoot(start: string): string`** — exported function. Walks up from `start` directory looking for `.git` or `.mdsroot` markers (same as Rust's `NativeFs::find_project_root`). Falls back to `start` if no marker found within `MAX_TRAVERSAL_DEPTH = 256` directories.

This determines the boundary for path traversal security and the base for computing virtual module keys (relative paths from project root).

### Key Design: entryFilename is a relative path

`buildModulesMap` computes:
- `projectRoot = findProjectRoot(dirname(absoluteEntry))` — discovered via `.git`/`.mdsroot` markers
- `entryFilename = relative(projectRoot, absoluteEntry)` — path from project root, not just `basename`

**Consequence**: `entryFilename` is now a path like `packages/mds/__test__/fixtures/imports/entry.mds`, not just `entry.mds`. Tests must use `endsWith` checks rather than equality when verifying `entryFilename`.

**Why this matters**: Cross-directory imports (e.g. `../lib/helpers.mds` from `app/entry.mds` to a sibling directory `lib/`) require `projectRoot` to be the common ancestor. Without project root discovery, the sibling directory import would be rejected as escaping the project root.

### `normalizeVirtualKey(base: string, relative: string): string`

Exported function. Mirrors Rust's `VirtualFs::normalize()` exactly. Converts a relative import path to a canonical slash-separated virtual key:
- `base = ''` (root entry): uses `relative` as-is (no parent resolution)
- `base != ''`: resolves `relative` against the directory of `base`
- `..` segments are allowed up to the project root; escaping throws `'import path escapes project directory'`
- Empty path throws `'import path is empty'`
- Null byte throws `'import path contains null byte'`
- Path with >256 segments throws segment-count error

**MUST exactly mirror the Rust implementation** — any divergence causes import resolution mismatches between the TypeScript scanner and the Rust WASM module.

### `buildModulesMap(entryPath, scanImports, options?): Promise<BuildModulesMapResult>`

Recursively resolves an MDS entry file and all its transitive imports into a flat modules map.

**Returns**: `{ entryFilename: string; modules: Record<string, string> }`

The `modules` map includes the entry file keyed by `entryFilename`. Callers that pass `modules` to WASM `compile`/`check` **MUST** extract and remove the entry source before the call — leaving the entry key present causes `mds::filename_collision` because WASM also inserts the entry source under `filename`.

**Security checks** (in order):
1. `validateImportPath(importPath, absoluteDir)` — rejects null bytes, empty paths, and paths escaping `projectRoot`
2. `openAndValidateModule(absolutePath)` — security perimeter:
   - `openNoFollow` — O_NOFOLLOW | O_RDONLY; `ELOOP`/`ENOTDIR` → security error about symlink
   - `handle.stat().isFile()` — rejects non-regular files (directories, devices, etc.)
   - `realpath` check — on platforms where O_NOFOLLOW=0 (Windows), compares resolved path; mismatch → security error
3. Aggregate size checked **before** reading content (via `fstat` size from the opened handle) to prevent forced allocation of content that will be rejected

**Resource limits**:
- `maxModules` (default: `DEFAULT_MAX_MODULES = 256`) — checked immediately after `visited.add()`
- `maxAggregateSize` (default: `DEFAULT_MAX_AGGREGATE_SIZE = 10 MiB`) — checked before `readFile`
- `MAX_IMPORT_DEPTH = 64` — explicit depth parameter on `scan()`, enforced before recursing

**Parallelism**: Child imports at each level are resolved with `Promise.all(importPaths.map(...))`. Aggregate size increments are safe because JS is single-threaded.

### Security Architecture

| Threat | Defense |
|---|---|
| Symlink traversal | `O_NOFOLLOW` (Linux/macOS) or post-open `realpath` comparison (Windows) |
| Path traversal above project root | `projectRoot` prefix check in `validateImportPath` and `openAndValidateModule` |
| Circular imports | `visited: Set<string>` of absolute paths |
| Import chain depth | `depth` parameter with `MAX_IMPORT_DEPTH = 64` guard |
| Excessive modules | `maxModules` guard after `visited.add()` |
| Excessive memory | `maxAggregateSize` checked before `readFile` via fstat |
| Non-regular files | `handle.stat().isFile()` check |
| Filesystem root as project root | Explicit rejection: `projectRoot === '/'` throws |
| Non-UTF-8 paths | Node.js handles UTF-8 natively; `node:path` functions work on string representations |

## Options Utility (`packages/mds/src/util/options.ts`)

`varsOpt(options?)` — returns `{ vars: Record<string, unknown> }` when `options.vars` is defined and non-null, `undefined` otherwise. Used by both `native.ts` and (indirectly) `wasm.ts` to avoid creating unnecessary option objects.

## Package Configuration

`packages/mds/package.json`:
- `"name": "@mdscript/mds"` (was `@mds/mds` before the repo rename to `dean0x/mdscript`)
- `"version": "0.2.0"` — coordinated release with all other workspace packages
- `"type": "module"` — ESM-only
- `"engines": { "node": ">=22.0.0" }` — requires Node 22+ (uses `node:test` runner and modern ESM)
- Exports: `"."` → `"node"` condition → `dist/node.js`; `"default"` → `dist/browser.js`
- `"dependencies": { "@mdscript/mds-wasm": "^0.2.0" }`
- `"optionalDependencies": { "@mdscript/mds-napi": "^0.2.0" }` — native addon is optional (WASM fallback if missing)
- Repository URL: `git+https://github.com/dean0x/mdscript.git` (updated from `dean0x/mds` at v0.2.0)
- Scripts: `test` (all tests), `test:native` (native backend only), `test:perf` (benchmarks)
- Build: `tsc -p tsconfig.json` → `dist/`

## Test Suite (`packages/mds/__test__/`)

Tests use Node.js built-in `node:test` runner. All tests require the built `dist/` output.

| File | Tests | Scope |
|---|---|---|
| `compile.spec.mjs` | U-C1–U-C9 | `compile()` behavior |
| `check.spec.mjs` | U-K1–U-KF3 | `check()` and `checkFile()` |
| `compile-messages.spec.mjs` | U-CM1–U-CM12 | `compileMessages()` behavior end-to-end |
| `compileFile.spec.mjs` | U-CF1–U-CF9 | `compileFile()` behavior |
| `wasm-compileFile.spec.mjs` | U-WCF1–U-WCF11 | WASM backend file ops (subprocess isolation) |
| `error.spec.mjs` | U-E1–U-E9 | Error shape: `code`, `help`, `span`, `isMdsError` |
| `backend.spec.mjs` | U-B1–U-B11 | Backend selection, MDS_BACKEND env, getBackend |
| `browser.spec.mjs` | U-BR1–U-BR13 | Browser entry pre/post-init, promise dedup, retry reset |
| `wasm-backend.spec.mjs` | U-WB1–U-WB21 | Circuit breaker, browser circuit breaker, shape validation (incl. compileMessages) |
| `native-backend.spec.mjs` | U-N1–U-N6 | Native backend isolation via `createNativeBackend` |
| `scanner.spec.mjs` | U-S1–U-S10, U-SM1–U-SM8 | `normalizeVirtualKey` and `buildModulesMap` |
| `perf.spec.mjs` | U-PF1–U-PF5 | Performance (no strict timing assertions) |

`helpers.mjs` exports shared fixture paths: `FIXTURES`, `SIMPLE_MDS`, `IMPORT_PROVIDER_MDS`, `IMPORT_CONSUMER_MDS`, `ENTRY_MDS`, `EMPTY_MDS`, `FRONTMATTER_ONLY_MDS`, `MD_EXTENSION`.

`wasm-compileFile.spec.mjs` uses subprocess isolation via `execFile` to prevent cross-test contamination of the module-level backend singleton. `wasmEnv()` / `nativeEnv()` build environment overrides. The `runScript(script, env)` helper spawns an inline ESM script and parses its JSON stdout.

### Scanner Test Fixtures

- `fixtures/imports/` — multi-file import chain: `entry.mds` → `lib.mds` → `deep.mds` (3+ modules)
- `fixtures/simple.mds` — single file with no imports
- `fixtures/cross-dir/app/entry.mds` — imports `../lib/helpers.mds` (sibling directory cross-dir test, U-SM8)
- `fixtures/cross-dir/lib/helpers.mds` — sibling directory module
- `fixtures/edge/` — edge cases: `empty.mds`, `frontmatter_only.mds`, `md_extension.md`

## Integration Guidelines

### Adding a New Public Function to Both Node and Browser

1. Add the function signature to `types.ts` (`MdsBaseBackend` for browser-safe, `MdsNodeBackend` for node-only).
2. Add the function to both `WasmModule` interface and `NapiAddon` interface (the underlying runtimes must expose it).
3. Add the adapter implementation to `createWasmBackend` in `backend/wasm.ts` and `createNativeBackend` in `backend/native.ts`.
4. If the new function requires WASM shape validation, add it to the `validateWasmShape` loop in `wasm.ts`.
5. Add the top-level export to `browser.ts` and `node.ts` (calling through `assertReady()`).
6. Export from `index.ts` if it should be re-exported.

`compileMessages` is the canonical example of this pattern — every layer was updated together.

### Using `findProjectRoot` for Path Computation

Any code that computes a virtual module key relative to a project boundary should:
1. Call `findProjectRoot(dirname(absolutePath))` to discover the boundary.
2. Use `relative(projectRoot, absolutePath)` to compute the virtual key.
3. Reject `projectRoot === '/'` as a filesystem root guard.

### Extending `buildModulesMap` Resource Limits

Add new limit constants at module scope (named `MAX_*`). Check limits at the earliest possible point — before I/O for size limits, after `visited.add()` for count limits. Document the limit in `ModuleScannerOptions` interface.

## Anti-Patterns

- **Calling `compile`/`check`/`compileMessages`/`compileFile`/`checkFile` before `init()`** — throws `'@mdscript/mds: call await init() ...'`. Always `await init()` first.
- **Passing `modules` that still contains the entry source to WASM `compile`** — causes `mds::filename_collision`. Extract and remove `modules[entryFilename]` before passing `modules` to the WASM call.
- **Hardcoding `entryFilename === basename(path)`** — `entryFilename` is `relative(projectRoot, absolutePath)`, which may include subdirectory segments. Use `endsWith` checks in tests and callers.
- **Using `createNativeBackend` with a direct `require('@mdscript/mds-napi')`** — the addon is injected for testability; direct require creates coupling that breaks test isolation.
- **Importing `buildModulesMap` in browser-side code** — it uses `node:fs/promises` and `node:path`; safe only in Node.js environments.
- **Comparing `nodeFailures >= MAX_INIT_RETRIES` in tests using a literal** — mirror `MAX_INIT_RETRIES` as a constant in the test file so that drift surfaces as a test failure rather than a silently wrong threshold.
- **Mutating the `DEFAULT_COMPILE_OPTS` object** — it's deep-frozen; attempting mutation in strict mode throws. Build a new options object via `compileOpts(options)` instead.
- **Using `file:` links in production code paths** — `@mdscript/mds-napi` is `file:` linked in `optionalDependencies` for local dev/CI; production consumers install the published npm package.
- **Referencing the old package name `@mds/mds` or repo `dean0x/mds`** — the package is `@mdscript/mds` and the repo is `dean0x/mdscript` as of v0.2.0.
- **Adding a new method to `MdsBaseBackend` without adding it to `validateWasmShape`** — the shape check will not catch WASM modules missing the new export, causing silent runtime failure.

## Gotchas

- **`entryFilename` includes directory segments** — since project root discovery was added, `entryFilename` is `relative(projectRoot, absoluteEntry)` not `basename(absoluteEntry)`. A file at `packages/mds/__test__/fixtures/imports/entry.mds` in a repo with `.git` at the workspace root will have `entryFilename` reflecting the full relative path.
- **`findProjectRoot` falls back to the starting directory** — if no `.git` or `.mdsroot` marker is found within `MAX_TRAVERSAL_DEPTH` parents, `findProjectRoot` returns `start`. This means files outside any recognized project boundary use their own directory as root, same as the previous behavior.
- **WASM `modules` map MUST NOT include the entry** — the WASM `compile`/`check` functions take `{ filename, modules, vars? }` where `filename` is already the entry. If `modules[filename]` also exists, the WASM backend raises `mds::filename_collision`. Remove the entry key before passing `modules`.
- **Circuit breaker is per-process** — `nodeFailures` and `browserFailures` are module-level singletons. Multiple tests in the same process share failure state. Use `_resetForTesting()` between tests.
- **`initWasmNode` defers `node:module` import** — the `import('node:module')` is inside the async function body, not at module scope. This keeps `wasm.ts` importable in browser environments (where `node:module` is unavailable).
- **`O_NOFOLLOW = 0` on Windows** — the symlink guard falls back to a post-open `realpath` comparison. This is a race-condition window (the symlink could be created between `open` and `realpath`), but it's the best available on Windows.
- **Aggregate size check uses fstat, not file content length** — `aggregateSize += fileSize` uses `stats.size` from `handle.stat()` before `readFile`. The actual UTF-8 decoded content may differ slightly from the byte count on some systems, but this is conservative and acceptable.
- **Subprocess isolation for WASM compileFile tests** — `wasm-compileFile.spec.mjs` spawns subprocesses to prevent the module-level `backend` singleton from being contaminated across test cases. Each subprocess gets a fresh module instance.
- **Node 22+ required** — tests use `node:test` built-in runner; `node:fs/promises` features require Node 22+. Running with Node 18 or 20 will fail.
- **`dist/` must be built before tests** — tests import from `../dist/`. Run `npm run build` in `packages/mds/` before running tests.
- **Package and repo renamed at v0.2.0** — npm package is `@mdscript/mds` (not `@mds/mds`), native addon is `@mdscript/mds-napi` (not `mds-napi`), repo is `dean0x/mdscript` (not `dean0x/mds`). Internal `loadNativeBackend()` uses `require('@mdscript/mds-napi')`.
- **`validateWasmShape` now requires `compileMessages`** — WASM modules built before Issue #56 will be rejected at init with a missing-export error. Must rebuild with `wasm-pack build crates/mds-wasm`.

## Related

- `crates/mds-napi/` — the native Node.js addon that `loadNativeBackend()` dynamically loads via `require('@mdscript/mds-napi')`. Changes to the napi error shape (`.code`, `.span`) or export signatures (including `compileMessages`) affect this package.
- `crates/mds-wasm/` — the WASM module that `initWasmNode`/`initWasmBrowser` loads. Changes to the `WasmModule` shape (especially `scanImports` and `compileMessages`) affect the module scanner and WASM backend.
- `crates/mds-core/src/fs.rs` — `VirtualFs::normalize()` must stay in sync with `normalizeVirtualKey()` in `module-scanner.ts`. Any change to how the Rust resolver normalizes import paths must be mirrored here.
- `packages/bundler-utils/` — exports `LazyInit<T>` used by Vite/Webpack/Rollup plugins for single-init guarantee and concurrent-call deduplication. Shares the same pattern as `ensureBackend`.
