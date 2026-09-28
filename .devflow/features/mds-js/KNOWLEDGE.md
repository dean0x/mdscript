---
feature: mds-js
name: "@mdscript/mds universal JS package — option forwarding, backends, published TS types"
description: "Use when modifying the JS/TS public API surface, adding backend methods, changing option types, debugging basePath rejection behaviour, changing result types, updating the backend contract, working on WASM/native backend validation, debugging why a backend result is rejected, or changing the WASM backend's JS file pre-scanner (module-scanner.ts / path-chars.ts / sliding-window.ts: path refusals, forbidden path characters, symlink and not-found handling, read-ahead concurrency, module aliases). Keywords: compileFile, compile, check, checkFile, lint, lintFile, lintVirtual, CompileResult, MarkdownResult, MessagesResult, CheckResult, LintResult, LintDiagnostic, LintFileOptions, CompileFileOptions, FileOptions, assertResultShape, validateBackendMethods, METHOD_KEYS, forwardOpts, assertKnownKeys, getBasePathError, BASEPATH_REJECTORS, BASE_METHODS, NODE_METHODS, WASM_EXPORTS, discriminated union, kind, mds::invalid_backend_result, mds::invalid_options, basePath, synchronous throw, native.ts, wasm.ts, contract.ts, types.ts, node.ts, browser.ts, options.ts, module-scanner.ts, path-chars.ts, sliding-window.ts, slidingWindow, buildModulesMap, ScannerEngine, ImportRecord, preflightModule, scanImportRecords, moduleAliases, aliasKey, withImportContext, assertKeyMatchesDisk, canonicalFile, findProjectRoot, readAtMost, ReadBudget, MAX_IMPORTS_READ_AHEAD, sanitizeControlCharsWire, escapePathForMessage, isForbiddenPathChar, forbiddenCharMessage, importPathViolation, PathError, realpathParent, openNoFollow, readError, cannotReadError, symlinkError, escapesProjectError, notRegularFileError, closeModule, mds::file_not_found, mds::io, mds::import, mds::resource_limit, O_NOFOLLOW, ELOOP, ENOTDIR, ENAMETOOLONG, EACCES, U-FP, U-SM, U-SW, U-WCF, #265, #408, #414, #417, #418, #424, #427, #428, #432."
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
  - packages/mds/src/util/module-scanner.ts
  - packages/mds/src/util/path-chars.ts
  - packages/mds/src/util/sliding-window.ts
  - packages/mds/__test__/scanner.spec.mjs
  - packages/mds/__test__/forbidden-path-chars.spec.mjs
  - packages/mds/__test__/wasm-compileFile.spec.mjs
  - packages/mds/__test__/compileFile.spec.mjs
created: 2026-06-26
updated: 2026-09-28
---

# @mdscript/mds Universal JS Package

## Overview

`packages/mds/` is the universal JS/TS package. It wraps either the native NAPI addon (Node.js) or the WASM module (browser/WASM Node) behind a unified API. The seven public methods are `compile`, `check`, `compileFile`, `checkFile`, `lint`, `lintFile`, and `lintVirtual`. Option validation is handled in a single table-driven choke point (`util/options.ts`) before any backend call. `compile`/`compileFile` return a discriminated union (`CompileResult = MarkdownResult | MessagesResult`), not a flat string.

The WASM backend has no filesystem, so `compileFile`/`checkFile`/`lintFile` on that backend first read the entry file and its transitive imports in JS (`util/module-scanner.ts`, `util/path-chars.ts`, `util/sliding-window.ts`) and hand the engine a virtual module map. As of Wave 1.5 (#414/#417/#418/#424/#427/#428) that pre-scanner is at **native parity**: every refusal it raises is coded with the native engine's error, message, `help` and, where native attaches one, `span` — the section below documents the architecture in detail.

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

**`assertKnownKeys(options, method)`** — throws `mds::invalid_options` synchronously if `options` contains any key not in `METHOD_KEYS[method]`. Message format matches `format_unknown_keys_error` in `mds-core/src/options.rs` byte-for-byte (enforced by U-OV-14 / U-OV-31 `strictEqual` against the live napi message). **Since #418, the offending key itself is WIRE-escaped with `sanitizeControlCharsWire` (from `path-chars.ts`) before it is quoted into the message** — a key carrying ESC, a bidi override, or another of the 80 forbidden codepoints can no longer inject terminal escapes or forge lines into whatever prints the error; a lone surrogate is shown as-is here (napi receives it decoded to U+FFFD, so the two messages can differ only in that one case). Key ORDER in `METHOD_KEYS` entries is load-bearing because it determines the `recognised keys are: …` list — do not reorder without verifying the napi order matches.

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

### SourceMapV3.sources ordering

`SourceMapV3.sources` names the caller-supplied entry identity at `sources[0]` — **unless the entry `@extends` a base**, in which case the chain root (the outermost `@extends` ancestor) comes first instead. This is a small but real ordering rule in `types.ts`'s doc comment; a consumer that assumes `sources[0]` is always the entry it passed in will mis-map the entry's own source for an extending template.

### File-surface type naming family

`CompileFileOptions` is the canonical interface. `FileOptions` is a `@deprecated` alias kept for backward compatibility. The naming family is now `CompileFileOptions`, `CheckFileOptions`, `LintFileOptions`.

`CompileFileOptions` deliberately does NOT extend `CompileOptions` — after `CompileOptions` gained `basePath`, inheritance would silently add an invalid field to the file surface. All file-surface option fields are declared directly.

`MdsBackend` (previously declared once, exported from neither entry point) is deleted. Do not re-introduce it.

### LintDiagnostic nullability

`LintDiagnostic.help`, `.span`, and `.fix_edits` are all **required** keys with `| null` types — no `?` (no `undefined` arm). As declared in `types.ts`:

- `help: string | null` — always present on the wire; `null` (never `undefined`) when the rule emits no hint
- `span: LintSpan | null` — always present on the wire; `null` when the rule produces no source span
- `fix_edits: Array<{ start: number; end: number; new_text: string }> | null` — always present on the wire; `null` when no auto-fix edits exist

**PR #335 (J2) reversed the prior `?:` shape.** The `?` was removed because backends always emit all three keys (serde serializes `Option::None` as JSON `null`), so `undefined` was never reachable at runtime. Guards must check `!== null`, NOT `!== undefined` — a `!== undefined` guard silently passes `null` through.

Consumer fixtures pin this invariant: `consumer-node.ts` carries `fix_edits: null` literals and three `@ts-expect-error` missing-key positive controls for `help`, `span`, and `fix_edits`; `consumer-browser.ts` carries the same for `fix_edits` (testing-03 revised). If any of the three fields regresses to optional (`?:`), tsc reports "Unused '@ts-expect-error' directive" and the type-check build fails.

### Export divergence: node.ts vs browser.ts

File-surface types (`CompileFileOptions`, `CheckFileOptions`, `FileOptions`, `InitOptions`) are exported from `node.ts` only — `browser.ts` has no file operations. This divergence is intentional. `LintFileOptions` IS exported from `browser.ts` (used by `lintVirtual`).

String-surface types (`CompileOptions`, `CheckOptions`, `LintOptions`) are shared between both entries and carry `basePath` on both (ADR-011). The WASM backend enforces the constraint at runtime with `mds::invalid_options`; no narrowed browser-only alias exists.

## Backend Layers

### NapiAddon interface (native.ts)

Covers all seven method names including `lint`, `lintFile`, `lintVirtual`. The lint file surface uses `NapiLintFileOpts = { vars?, rules? }` — no `basePath`. The lint string surface uses `NapiLintOpts = { basePath?, vars?, rules? }`.

### WasmModule interface (wasm.ts)

Exports `compile`, `check`, `lint`, `lintVirtual`, `scanImportRecords`, `preflightModule`. `lintFile` is NOT in `WasmModule` — file operations are added via `wrapWithFileOps` in `node.ts`, which reads modules through `buildModulesMap` and then calls `wasmModule.compile`/`check`/`lint` directly with a `modules`/`moduleAliases` option.

`compile`/`check`/`lint` on `WasmModule` all accept `moduleAliases?: Record<string, string>` — the keys imports reach a module by other than its own on-disk key (case variants, symlinked-directory spellings). `fileOpts()` and the `lintFile` path in `node.ts` only include this key when `aliases` is non-empty.

`ImportRecord` (the shape `scanImportRecords` returns) is declared in `backend/wasm.ts` beside `WasmModule` — not in `module-scanner.ts`, which only `import type`s it. `scanImports` (the older, path-only export) is **removed** from both `WASM_EXPORTS` and the `WasmModule` interface; the Rust export and the `mds-wasm` README entry still exist (the engine exports more than `WasmModule` requires), just unused by this package now.

`lintVirtual` on the WASM backend deliberately OMITS the `basePath` guard — `LintFileOptions.basePath` is `never` and the public wrapper's `BASEPATH_REJECTORS` rejects it first. That omission applies ADR-011 and should NOT be "fixed".

### Defense in depth across backends

`createNativeBackend` and `createWasmBackend`'s `wrapWithFileOps` each carry per-method `basePath` guards (avoids PF-004) in addition to the public wrapper's, covering `compileFile`, `checkFile`, `lintFile`, and `lintVirtual`. An internal caller that obtains a backend directly and bypasses the public wrapper's `BASEPATH_REJECTORS` is caught here. Without these backend-level guards, `forwardOpts` would silently drop `basePath` on native (since it's absent from `METHOD_KEYS` for those surfaces) while WASM would throw — producing asymmetric behavior on the same call.

## WASM-backend File Pre-scanner (`util/module-scanner.ts`, `util/path-chars.ts`, `util/sliding-window.ts`)

The scanner is a second, JS implementation of the native backend's path rules; its refusals must match the native engine's error CODE, MESSAGE, `help`, and — where native attaches one — `span`, byte for byte. `ScannerEngine = Pick<WasmModule, 'scanImportRecords' | 'preflightModule'>` — the only two WASM engine calls the scanner makes; every other check (paths, symlinks, project root, limits, import context) is a TypeScript mirror of the native backend's, held to it by native-vs-WASM differentials.

### Engine calls

- **`preflightModule(bytes, display, typed)`** wraps the core's `check_module_bytes` (per-file cap, then UTF-8 decode, BOM kept) then `check_module_type` (`not_mds`, judged on `display`'s extension, named in the error by `typed`) — the same order and the same checks `NativeFs::read` makes on every file. `readModule` calls it with `display = keyOf(projectRoot, resolved)` (the module's on-disk-spelled, root-relative key) and `shown` (the path as written) as `typed`.
- **`scanImportRecords(source, asBase)`** returns `ImportRecord[]` — `{ path, kind: 'extends'|'frontmatter'|'import'|'export-from', frontmatterIndex: number|null, span: MdsErrorSpan|null }` — in the order the native resolver resolves them for the way the module is reached: as another module's `@extends` base (`asBase = true`, its own `@extends` listed *after* its imports, mirroring how the resolver walks a base) or for itself (`asBase = false`, matching `scan_imports`' own order). The walk passes `asBase` down from `record.kind === 'extends'` on the *parent's* record, so a base three levels deep still gets the right order.

### Deterministic order despite concurrent reads

Modules are **read concurrently** — at most `MAX_CONCURRENT_OPENS` (16) files open at once, at most `MAX_IMPORTS_READ_AHEAD` (`2 × MAX_CONCURRENT_OPENS` = 32) of a module's imports read ahead of the walk via the `slidingWindow` generator (`sliding-window.ts`) — but **checked in the native resolver's order**: depth-first, each module's imports in `scanImportRecords`' order, first-fault-wins matching what native reports for the same graph (U-SM37/U-SM38, difference 8). `slidingWindow(items, size, start)` hands out `items` one at a time, keeping up to `size` `start()` calls in flight ahead of the one just yielded — refilled *after* each yield, not before, so the walk's own pace throttles how far ahead reads run (U-SW1–U-SW4 pin exact fill/yield timing; `size` must be a positive integer or it throws `RangeError`).

Each import walked starts a `readAhead()` for it via `startReadAhead`, tracked in a `pending` Set<Promise> so a walk that throws can wait for every outstanding read-ahead to settle before rethrowing (none holds a file open or calls into the engine after the walk finishes — `walkFinished` flag plus `Promise.allSettled(pending)` in the `finally`). `readAheadReads` (a `Map<key, Promise>`) deduplicates: at most one read-ahead per module key, whichever import reaches it first; a module the walk itself reaches later reuses that settled read (`readAheadRead(key)`) instead of reading it twice.

### Byte-charged read-ahead budget

`ReadBudget` (`admit(size): boolean`, `charge(bytes): void`) gates how much the read-ahead may pull into memory before the walk gets there: each module is *admitted* by its `fstat` size against `maxAggregateSize` (10 MiB default), then *charged* the true bytes read once the read completes — a file can grow between its fstat and its read, so the charge can exceed the admission (bounded by `MAX_FILE_SIZE + 1`, the per-file cap-plus-one that `readAtMost` enforces). A module the budget declines to admit ahead of time (`admit` returns `false`) is read later by the walk itself, uncounted against the read-ahead budget — the walk's own `aggregateSize` accumulator (checked AFTER `preflightModule`, never before) is the one guard that can refuse it, and only once every native check on that module has already passed (difference 2 below).

### Error construction and context

- **`pathError(code, message)`** builds the base shape; `PATH_ERROR_HELP` attaches `help` only for `mds::file_not_found` (`'check the file path and ensure the file exists'`) — every other code carries none, on either backend. A `PathError`'s own enumerable keys are exactly `code` (+ `help` when file-not-found, + `span` only once `withImportContext` attaches one) — pinned by U-SM49; do NOT unconditionally set `err.help` for every code.
- **`refusalParts`, a `WeakMap<Error, RefusalParts>`**, records what `importError`/`fileNotFoundError` built the message from (the detail string, or the `shown` path) — never re-parsed out of the message text. `withImportContext(err, record)` reads it to rebuild the error the way the native resolver's `attach_import_span`/`attach_frontmatter_index` do: a frontmatter import's `mds::file_not_found` becomes `mds::import` with `(in frontmatter imports[<i>])` appended; an `mds::import` error not already inside a frontmatter context gets the suffix too; a plain `@import`/`@extends` miss gets the directive's `span` attached. Only errors with no `span` yet and a `refusalParts` entry are touched — the engine's own errors (from `preflightModule`/`scanImportRecords`) are passed through untouched.
- **`nativeParentAndName(path)`** mirrors Rust `Path::parent`/`Path::file_name`: repeated/trailing separators and `.` components don't count, `..` is kept as written (the OS applies it during canonicalization — after symlinks on POSIX, lexically on Windows). `findProjectRoot`/`canonicalFile`/`realpathParent` are the canonicalization primitives everything else composes.

### Filesystem checks

- **Symlinks**: `assertFileNotSymlink` + `canonicalFile` mirror `NativeFs::check_symlink_named` — refuse by the final component's OWN file type (`openNoFollow`'s `O_NOFOLLOW` → `ELOOP`, or the post-open `lstat` where `O_NOFOLLOW` is unavailable, i.e. Windows, which also reports junctions), never by comparing the canonical path with the path as written (a case-mismatched name on a case-insensitive volume is NOT a symlink, #408).
- **Non-UTF-8**: `resolvePath` reads the OS's raw bytes (`realpath(path, { encoding: 'buffer' })`) and refuses (`notUtf8Error`, `mds::io`, `resolved path is not valid UTF-8: "<path as written>"`) rather than decoding lossily — a lossy decode would name a different, valid-UTF-8 "twin" path that no check here has seen (#414).
- **Non-regular file**: `readModule` `lstat`s the path before opening it — a directory, device, FIFO, or socket is refused (`notRegularFileError`, `mds::io`, `cannot read <path>: not a regular file`) before it is ever opened, so a FIFO nobody writes to cannot block the scan (#428). The check is repeated on the opened fd's `stat()` (TOCTOU: the component may have changed between the `lstat` and the `open`).
- **`assertKeyMatchesDisk`** (#408) refuses an import whose virtual key names a different directory than the one native reads it from: the WASM engine resolves an import BY NAME from the importing module's key, native resolves it ON DISK from the importing module's canonical directory — they disagree exactly when an import leaves a symlinked directory through `..` (`./link/../x.mds`; the OS applies `..` AFTER the link on POSIX, the key-based resolution applies it before). Mismatch throws `mds::import` (`import path leaves a symlinked directory through '..', which the WASM backend cannot resolve: "<path>"`).
- **Bounded reads**: `readAtMost(handle, limit, sizeHint)` grows a buffer by doubling (floor `MIN_READ_GROWTH` = 64 KiB) up to `limit` (`MAX_FILE_SIZE + 1`), so a file that grows while being read is never held whole and never more than one byte past the cap reaches the engine (#428).

### Module identity and aliases

Every module is keyed by its **canonical path below the project root, in its on-disk spelling** — the same key native uses, so `dependencies`, `sourceMap.sources` and a cycle's text name it that way whatever spelling reached it. `Located.aliasKey` is the key the engine would resolve the import to BY NAME (before any alias remaps it); when that differs from `Located.key` (a case variant on a case-insensitive volume, or a path through a symlinked directory), `recordAlias` records `aliasKey → key` in the scan's `aliases` map, returned as `BuildModulesMapResult.aliases` and passed to the WASM engine as the `moduleAliases` option (`fileOpts`, the `lintFile` path in `node.ts`). The `walking`/`completed` sets (native's resolving-stack and module-cache) are keyed by `key`, so a module reached under two spellings is read and checked exactly once. The engine itself caps `moduleAliases` at 65,536 entries totaling 10 MiB (`mds::resource_limit`) — the JS scanner does not pre-count this; a graph with more distinct alias spellings than that compiles only on native (difference 9).

### The ten remaining differences (JSDoc on `buildModulesMap`, residual tracked by #432)

1. A MISSING module outside the project root (lexically, or through a symlinked directory) is refused as escaping it; native reports it not found — deliberate, containment is decided before the file is looked at (U-SM30, U-SM32).
2. The aggregate-size guard (`maxAggregateSize`) is the WASM backend's own; it never pre-empts a native refusal of that module (U-SM39).
3. A `../` import from a project rooted at the filesystem root is refused as escaping it; native resolves `/..` to `/` (#424; U-S16, U-SM34 container arm).
4. Module count: native refuses once 256 OTHER modules are fully resolved; the engine takes the entry plus 256 admitted, so a larger in-flight graph can be refused here that native would still be resolving (#427).
5. On POSIX, an import that names a symlinked directory and leaves it again through `..` is refused (see `assertKeyMatchesDisk` above) — narrowed to exactly this shape; Windows collapses `link\..` lexically before following the link on both backends, so it compiles there on both (#408).
6. A module more than 256 directories below the project root is refused (the engine's key is capped at 256 segments); native caps only the path as written.
7. A `cannot read` message names the path as written where native names the root-relative path, and an errno name where native gives the OS error text; the `code` agrees on both (U-SM28, U-SM31, U-SM33 `./`).
8. Every module is read before the engine runs, so a refusal of a later import can be reported before an error the engine would raise at a point native reaches first — a compile error in a module native reads first, or a circular import native meets first (U-SM38).
9. The engine caps `moduleAliases` at 65,536 entries / 10 MiB and refuses more; native has no aliases to count.
10. A module below a directory whose canonical path is not valid UTF-8 is refused as soon as that directory resolves, before the file is looked at; native refuses the same error only once it has checked the file, so where the file is ALSO missing, a symlink, escapes the root, or has a forbidden character, native reports that instead (U-SM48, Linux-only — APFS refuses such bytes at the filesystem level).

### Tests

`__test__/forbidden-path-chars.spec.mjs` (U-FP1–U-FP5) is the Rust↔JS differential over all 80 forbidden codepoints. `__test__/scanner.spec.mjs` is the bulk of the parity suite: U-S11–16 (`normalizeVirtualKey`), U-SM3/7/9–37 (the original #265/#408 symlink/not-found/I/O matrix), U-SM38 (cycle-vs-missing-import ordering), U-SM39 (aggregate-vs-preflight order), U-SM40–42 (frontmatter/`@extends`-chain context), U-SM43/U-SM46 (simulated fstat-then-grows races, via a patched `FileHandle.prototype.stat`/`read`), U-SM44 (open-descriptor accounting after a mid-walk error, skipped on win32), U-SM45/47 (one `preflightModule`/`realpath` call per display, no duplicate reads), U-SM48 (non-UTF-8 directory, Linux-only), U-SM49 (exact own-enumerable-field set per refusal shape), U-SM50 (`_unwrapWalkFinishedForTesting()` throws an `Error`, never returns `undefined`), U-SM34 (root-project arm, `# SKIP` unless run as root in a throwaway container — not exercised outside a Linux container locally). `__test__/wasm-compileFile.spec.mjs` U-WCF12–14 compare whole compiled output (dependencies, `sourceMap.sources`, cycle text) between native and WASM on case-variant and symlinked-directory fixtures. `__test__/sliding-window` behaviour (U-SW1–4) lives inline in `scanner.spec.mjs`. The WASM side of any differential loads `crates/mds-wasm/pkg/` first — **rebuild it from the current tree before running these** (PF-035).

## Synchronous Throws Contract

`compileFile`, `checkFile`, and `lintFile` are non-`async` functions that return Promises. All option-validation errors (unknown keys AND basePath) throw **synchronously**, before any I/O. Callers using `try { compileFile(f, opts) } catch` capture both error classes synchronously. `.catch()` on the returned promise does NOT receive option-validation errors.

Tests for this must use `assert.throws` (not `assert.rejects`) — a regression to async throw would escape a `rejects` validator and surface as a test failure with misleading `testCodeFailure`, masking the regression (see U-OV-32/33).

`lintVirtual` is synchronous (no I/O), so this distinction does not apply to it.

## Method Manifests in contract.ts

```typescript
export const BASE_METHODS = ['compile', 'check', 'lint'] as const;
export const NODE_METHODS = ['compileFile', 'checkFile', 'lintFile'] as const;
export const WASM_EXPORTS = [
  ...BASE_METHODS,
  'lintVirtual',
  'scanImportRecords',
  'preflightModule',
] as const;
```

`BASE_METHODS` now includes `lint` (base backends share it), and `NODE_METHODS` now includes `lintFile`. `WASM_EXPORTS` no longer lists the old path-only `scanImports` export — it requires `scanImportRecords` and `preflightModule` instead, the two calls the file pre-scanner actually makes; the WASM module MAY export more (e.g. the engine's own `scanImports` still exists), but only the names in `WASM_EXPORTS` are required by `validateBackendMethods`.

`createNativeBackend` validates `[...BASE_METHODS, ...NODE_METHODS]` plus `lintVirtual`. Missing method throws a plain `Error` (not `mds::` coded) naming the missing method.

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
- **Including `compileMessages` or `scanImports` in `WASM_EXPORTS` or `BASE_METHODS`** — `compileMessages` was deleted from the WASM binding; `scanImports` was replaced by `scanImportRecords` (#414) and is no longer required.
- **Returning `{ kind: 'markdown', output: ..., messages: [] }` from mocks** — inactive field must be ABSENT; `assertResultShape` rejects it.
- **Adding a basePath guard to WASM `lintVirtual`** — the public wrapper fires first; the guard in `lintVirtual` is deliberately absent (ADR-011).
- **Reordering METHOD_KEYS witness literals** — key order determines the `recognised keys are: …` list; U-OV-14/U-OV-31 pins it via `strictEqual` against live napi, not a hardcoded string.
- **Unconditionally setting `err.help` on every `PathError`** — only `mds::file_not_found` carries one on either backend; U-SM49 asserts the exact own-key set per refusal shape.
- **Comparing a canonical path with the path as written to detect a symlink** — misreports a case-mismatched spelling on a case-insensitive volume as a symlink (#408). Judge the final component's own file type instead (`O_NOFOLLOW`/`lstat`).
- **Reading a module's full imports without a bound on concurrency or read-ahead** — a module with thousands of imports would hold thousands of pending file descriptors; `MAX_CONCURRENT_OPENS`/`MAX_IMPORTS_READ_AHEAD`/`slidingWindow` exist specifically to cap this.

## Gotchas

- **`lintFile`/`lintVirtual` on the file surface throw for basePath — use `assert.throws`, not `assert.rejects`** — this is the same synchronous-throw channel as `compileFile`/`checkFile`.
- **Type fixtures import from `../../dist/node.js`** — `consumer-node.ts` and `consumer-browser.ts` are compiled by `tsc -p tsconfig.types.json` and import from `dist/`. A stale or missing `dist/` silently typechecks yesterday's declarations. Build before running type tests.
- **Inferred-object shape `{ basePath: '/' }` IS rejected by file-surface types** — despite what early PR descriptions claimed, `TS2322` fires: `string` is not assignable to `undefined` (the effective type of `basePath?: never`). Consumer-node.ts lines 95-97 encode this.
- **Cross-surface differential tests hard-fail under `process.env.CI` when CLI/WASM is missing, but warn-and-return locally** — a green local run is NOT evidence; only CI is.
- **`CheckResult` has only `{ warnings: string[] }` — no `dependencies`** — this matches the napi wire; check does not expose deps.
- **The `assertReady()` error message is byte-literal** — `'@mdscript/mds: call await init() before using compile/check/compileFile/checkFile/getBackend'` — tests pin it; don't change without updating tests.
- **`basePath: undefined` is treated as "key absent"** — `getBasePathError` uses `!== undefined`, not `!= null`. Both backends must agree (enforced by U-OV-29). This is consistent with `forwardOpts`'s `!= null` filter that prevents `{basePath: undefined}` from reaching napi.
- **Option key lists live in four independent places** (TS `METHOD_KEYS`, napi, wasm, Python). TS↔napi parity is runtime-enforced; WASM and Python drift is caught only by prose review until #311 lands.
- **The scanner's "cannot read" message and native's disagree on wording, not code** — the scanner names the path as written and an errno name (`EACCES`), native names the root-relative path and the OS error text; only `code` should be compared cross-backend for that shape (difference 7).
- **`U-SM34` (root-rooted project) needs an actual root filesystem to run meaningfully** — it `# SKIP`s outside a throwaway container; a green local run without root access proves nothing about that arm.
- **`U-SM48` (non-UTF-8 directory name) only runs on Linux** — APFS (macOS) and NTFS/Windows both refuse to create such a filename, so the fixture cannot even be built there; it is skipped by design on darwin/win32.
- **Symlink `..` behaviour is NOT universally POSIX-only** — Windows collapses `link\..` lexically before following the link on BOTH backends, so that one import shape compiles identically everywhere; only the POSIX after-the-link semantics produce a WASM-vs-native divergence (difference 5). Do not describe the whole symlink-`..` story as "POSIX-only" in new docs.
- **A directory/FIFO/device/socket module is `not a regular file`, not `EISDIR`** — the old raw-errno message (`EISDIR`) is gone; `notRegularFileError` gives the same `mds::io` shape on every OS, checked before the file is opened (#428).

## Key Files

- `packages/mds/src/types.ts` — all public types: `Message`, `MarkdownResult`, `MessagesResult`, `CompileResult`, `CheckResult`, `CompileOptions`, `CheckOptions`, `CompileFileOptions` (canonical), `FileOptions` (deprecated alias), `CheckFileOptions`, `LintOptions`, `LintFileOptions`, `LintResult`, `LintDiagnostic`, `LintSpan`, `SourceMapV3`, `MdsBaseBackend`, `MdsNodeBackend`
- `packages/mds/src/util/options.ts` — `METHOD_KEYS`, `forwardOpts`, `assertKnownKeys` (now WIRE-escapes unknown keys), `getBasePathError`, `BASEPATH_REJECTORS`, `MethodName`
- `packages/mds/src/backend/contract.ts` — `BASE_METHODS`, `NODE_METHODS`, `WASM_EXPORTS`, `ResultKind`, `assertResultShape`, `validateBackendMethods`
- `packages/mds/src/backend/native.ts` — `NapiAddon` interface, `createNativeBackend` (with per-method depth-in-defense basePath guards)
- `packages/mds/src/backend/wasm.ts` — `WasmModule` interface, `ImportRecord`, `createWasmBackend`, `fileOpts`, WASM basePath error
- `packages/mds/src/node.ts` — `wrapWithFileOps`, all seven public functions, type re-exports
- `packages/mds/src/browser.ts` — browser-safe re-exports; no file operations
- `packages/mds/src/util/module-scanner.ts` — `buildModulesMap`, `ScannerEngine`, `normalizeVirtualKey`, `PathError`, `findProjectRoot`, `canonicalFile`, `realpathParent`, `openNoFollow`, `readAtMost`, `cannotReadError`, `readError`, `symlinkError`, `escapesProjectError`, `notRegularFileError`, `closeModule`, `assertKeyMatchesDisk`, `withImportContext` (WASM-backend file pre-scanner)
- `packages/mds/src/util/path-chars.ts` — JS mirror of the #265 forbidden-path-character rule; `sanitizeControlCharsWire`, `escapePathForMessage`, `isForbiddenPathChar`, `forbiddenCharMessage`, `importPathViolation`
- `packages/mds/src/util/sliding-window.ts` — `slidingWindow`, the read-ahead concurrency generator
- `packages/mds/__test__/options-validation.spec.mjs` — U-OV-1..U-OV-36; option validation, basePath forwarding, synchronous-throw contract, byte-identical message parity
- `packages/mds/__test__/scanner.spec.mjs` — U-S11–16, U-SM3–50, U-SW1–4; the native-vs-WASM pre-scanner differential
- `packages/mds/__test__/wasm-compileFile.spec.mjs` — U-WCF1–14; whole-shape compiled-output parity (dependencies, sourceMap.sources, cycle text)
- `packages/mds/__test__/forbidden-path-chars.spec.mjs` — U-FP1–U-FP5; the 80-codepoint differential
- `packages/mds/__test__/types/consumer-node.ts` — AC-P3-20/21 type-level matrix for Node entry
- `packages/mds/__test__/types/consumer-browser.ts` — AC-P3-16/20 type-level matrix for browser entry

## Related

- ADR-011 — String-surface option types are shared between Node.js and browser entries; `basePath` enforcement on WASM is runtime-only, not type-level. AC-P3-20 requires `basePath` to be a POSITIVE case in `consumer-browser.ts`.
- PF-004 (avoids) — Alternate code path silently bypassing an enforcement point. The per-backend depth-in-defense basePath guards, the unified `forwardOpts` replacing per-surface builders, and the aggregate-size guard checked strictly after native's own checks pass, all apply this lesson.
- PF-013 (avoids) — Vacuous absence-only assertions. U-OV-22 (wrong basePath throws, right basePath succeeds), U-OV-14/U-OV-31 (`strictEqual` against live napi), and the scanner's mutation-tested REDs (U-SM43/46/47's held-file, no-growth, and seen-call controls) apply this lesson.
- PF-007 (applies) — Per-surface goldens each lock in their own value; the native-vs-WASM differentials (U-SM21, U-SM32, U-SM37, U-WCF12–14) hold the scanner to native's own golden output, not a hardcoded string.
- PF-033 (applies) — A host-path leak fixed on only the one surface where it was reported recurs elsewhere; the WASM pre-scanner's path-escaping fixes (#265, #408) were swept across every message-building helper in `module-scanner.ts`/`path-chars.ts`, not just the one that first surfaced a leak.
- PF-035 (avoids) — Rebuild `crates/mds-wasm/pkg/` before running any native-vs-WASM differential; a stale `pkg/` silently exercises last commit's engine behaviour.
- PF-036 (applies) — U-SM34 (root project) and U-SM48 (non-UTF-8 directory name) have no meaningful local counterpart on a non-root user or on macOS/Windows respectively; they are Linux-container/CI-only checks, not locally reproducible gates.
- Feature: mds-napi — the native backend; compile/compileFile return the discriminated union this package types
- Feature: mds-lint — the lint surface; `LintResult`, `LintDiagnostic`, `LintFileOptions` types here are the wire contract for mds-napi's lint output
- Feature: bundler-plugins — imports `compileFile` from this package via `MdsApi` in `bundler-utils/src/types.ts`
- Feature: mds-compiler — the Rust `CompileResult`/`CompiledOutput` types, `check_module_bytes`/`check_module_type`, `scan_import_records`, and `VirtualFs::with_aliases` that this package's pre-scanner mirrors
