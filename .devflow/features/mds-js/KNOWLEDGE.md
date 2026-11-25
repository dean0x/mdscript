---
feature: mds-js
name: "@mdscript/mds universal JS package — option forwarding, backends, published TS types"
description: "Use when changing the @mdscript/mds JS API, option or result types, the backend contract, or the WASM backend's file and local data pre-scanner. Keywords: compileFile, buildModulesMap, walkData, preflightData."
keywords: "compileFile, compile, check, checkFile, lint, lintFile, lintVirtual, CompileResult, MarkdownResult, MessagesResult, CheckResult, LintResult, LintDiagnostic, LintFileOptions, CompileFileOptions, assertResultShape, validateBackendMethods, METHOD_KEYS, forwardOpts, assertKnownKeys, getBasePathError, BASEPATH_REJECTORS, BASE_METHODS, NODE_METHODS, WASM_EXPORTS, discriminated union, kind, mds::invalid_backend_result, mds::invalid_options, basePath, synchronous throw, native.ts, wasm.ts, contract.ts, types.ts, node.ts, browser.ts, options.ts, module-scanner.ts, path-chars.ts, sliding-window.ts, slidingWindow, buildModulesMap, ScannerEngine, ImportRecord, preflightModule, scanImportRecords, moduleAliases, aliasKey, aliasRefusal, withImportContext, canonicalFile, findProjectRoot, readAtMost, ReadBudget, MAX_IMPORTS_READ_AHEAD, sanitizeControlCharsWire, escapePathForMessage, isForbiddenPathChar, forbiddenCharMessage, importPathViolation, PathError, openNoFollow, readError, cannotReadError, symlinkError, escapesProjectError, notRegularFileError, closeModule, mds::file_not_found, mds::io, mds::import, mds::resource_limit, O_NOFOLLOW, ELOOP, ENOTDIR, ENAMETOOLONG, EACCES, U-FP, U-SM, U-SW, U-WCF, #265, #408, #414, #417, #418, #424, #427, #428, #432, #479, #483, local data, data:, walkData, locateData, dataRefusals, restoreDataRefusal, dataPathRefusal, dataAliasKey, rootlessDataKeyRefusal, check_key, preflightData, readData, MAX_DATA_FILES, MAX_DATA_BYTES, dataBound, FileIdentity, Located.id, fileChangedError, O_NONBLOCK, data-path-vectors.json, TRIMS_SEGMENTS, #405."
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
  - packages/mds/src/public-types.ts
  - packages/mds/__test__/export-surface.spec.mjs
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
  - packages/mds/__test__/backend-contract.spec.mjs
  - packages/mds/__test__/data-scanner.spec.mjs
  - packages/mds/__test__/data.spec.mjs
  - packages/bundler-utils/__test__/data-deps.spec.mjs
created: 2026-06-26
updated: 2026-11-25
---

# @mdscript/mds Universal JS Package

## Rules
- **KB-AP-1** Do not add per-surface option builders: their own key lists drift from `METHOD_KEYS` with no compile error.
- **KB-AP-2** Do not reject `basePath` in only some file-surface methods: all four use `BASEPATH_REJECTORS` with the napi message text.
- **KB-AP-3** Do not test an option-validation refusal with `.catch()` or `assert.rejects`: the throw is synchronous and escapes both.
- **KB-AP-4** Do not read `result.output` without a branch on `result.kind`: `CompileResult` is a union.
- **KB-AP-5** Do not put `compileMessages` or `scanImports` in `WASM_EXPORTS` or `BASE_METHODS`: neither is a required export now.
- **KB-AP-6** Do not return the inactive result field from a mock (`messages: []` beside `output`): `assertResultShape` rejects it.
- **KB-AP-7** Do not add a `basePath` guard to WASM `lintVirtual`: the public wrapper refuses it first, by design.
- **KB-AP-8** Do not reorder the `METHOD_KEYS` witness literals: the order is the `recognised keys are:` list that tests compare with live napi.
- **KB-AP-9** Do not set `err.help` on every `PathError`: only `mds::file_not_found` has a help text, on both backends.
- **KB-AP-10** Do not find a symlink by a comparison of the canonical path with the path as written: a case variant is not a link. Ask the file type.
- **KB-AP-11** Do not read the imports or data files of a module without a bound on open files and read-ahead: use `slidingWindow` and the read-ahead budget.
- **KB-AP-12** Do not keep a data refusal under one key while a later path can fill that key: the engine binds the later file to the refused path, and the refusal is lost.
- **KB-AP-13** Do not use the import context for the data refusals of an `@extends` base: native binds base data in the scope of the child.
- **KB-AP-14** Do not run only the portable rows of a shared vector table on a platform where the surfaces differ: the platform rows are the ones that can fail there.
- **KB-AP-15** Do not key a data path with the Windows segment trim: the wasm32 engine has no trim, so a path that only the trim refuses still has an engine key.
- **KB-AP-16** Do not trust path checks to name the opened file: compare the device and inode of the handle with `Located.id` before a byte is read.
- **KB-INV-1** `compileFile`, `checkFile` and `lintFile` throw every option-validation error synchronously, before any I/O.
- **KB-INV-2** `assertResultShape` reads no array element (`Array.isArray` only), and the inactive field of a `CompileResult` is absent.
- **KB-INV-3** `validateBackendMethods` requires exactly the names in `WASM_EXPORTS`, `preflightData` included; the WASM module can export more.
- **KB-INV-4** The scan throws no refusal of a new data key: the file stays out of `modules`, the refusal goes in `dataRefusals`, and `compileFile` restores it with `restoreDataRefusal`.
- **KB-INV-5** Only `compileFile` reads data (`readData` true). `checkFile` and `lintFile` pass false, and their data records are never located.
- **KB-INV-6** Data reads and module reads use separate read-ahead maps, so a file read by one rule never serves the other.
- **KB-INV-7** The data bounds (`MAX_DATA_FILES`, `MAX_DATA_BYTES`) count apart from modules; after the walk reaches one, no new data read starts.
- **KB-INV-8** One key never names two files: a second path of a known module or data key must name the same file, or it gets the alias conflict refusal.
- **KB-INV-9** The scan waits for every read-ahead to settle before it returns or throws.
- **KB-INV-10** A new import form changes `scan_import_records` and the walk together: the walk must see every path that the engine resolves.

## Overview

`packages/mds/` is the universal JS/TS package. It wraps either the native NAPI addon (Node.js) or the WASM module (browser/WASM Node) behind a unified API. The seven public methods are `compile`, `check`, `compileFile`, `checkFile`, `lint`, `lintFile`, and `lintVirtual`. Option validation is handled in a single table-driven choke point (`util/options.ts`) before any backend call. `compile`/`compileFile` return a discriminated union (`CompileResult = MarkdownResult | MessagesResult`), not a flat string.

The WASM backend has no filesystem, so `compileFile`/`checkFile`/`lintFile` on that backend first read the entry file and its transitive imports in JS (`util/module-scanner.ts`, `util/path-chars.ts`, `util/sliding-window.ts`) and hand the engine a virtual module map. For `compileFile`, it also reads the local data files of each module's `data:` entries (#405) by the native data rules, and keeps each data refusal for the compile to give back where native gives it. The pre-scanner mirrors native refusal codes, messages, `help` and spans for the tested cases. Known differences remain below.

## Core Responsibilities

- Export all seven public methods and `init`, `getBackend`, `isMdsError` to consumers
- Validate options against per-method key lists (`METHOD_KEYS`) before backend dispatch
- Reject `basePath` on file-surface methods with purpose-built errors whose messages are byte-identical to napi
- Validate that the loaded backend exposes the required method names
- Validate each method call returns the correct result shape (shallow O(1) check)
- Re-export shared public types from both entry points. Export `CompileFileOptions` and `CheckFileOptions` only from `node.ts`.
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

`tsconfig.base.json` enables `exactOptionalPropertyTypes` (EOPT). The consumer's own flag controls the type check. With EOPT on, `basePath?: never` requires an absent key. With EOPT off, this declaration accepts an explicit `undefined` value. The runtime guard is the primary control in both cases.

For file and virtual methods, `getBasePathError` rejects all `basePath` values except `undefined`, so it also rejects explicit `null`. `forwardOpts` omits nullish values, so the napi addon does not receive `basePath: undefined`. U-OV-29 requires both the native and WASM backends to accept `basePath: undefined` on all seven methods: `compile`, `check`, `lint`, `compileFile`, `checkFile`, `lintFile`, and `lintVirtual`.

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

### File-surface options

Use `CompileFileOptions` for `compileFile`, `CheckFileOptions` for `checkFile`, and `LintFileOptions` for `lintFile` and `lintVirtual`.

Declare file option fields directly. Do not extend `CompileOptions` in `CompileFileOptions` because `CompileOptions` accepts `basePath`.

`MdsBackend` (previously declared once, exported from neither entry point) is deleted. Do not re-introduce it.

### Optional input fields

Add `| undefined` to optional public input fields such as `vars` and `wasmUrl`. Do not add it to `basePath?: never`, output fields, or internal napi and WASM option fields. `forwardOpts` omits nullish values before the backend call.

### LintDiagnostic nullability

`LintDiagnostic.help`, `.span`, and `.fix_edits` are all **required** keys with `| null` types — no `?` (no `undefined` arm). As declared in `types.ts`:

- `help: string | null` — always present on the wire; `null` (never `undefined`) when the rule emits no hint
- `span: LintSpan | null` — always present on the wire; `null` when the rule produces no source span
- `fix_edits: Array<{ start: number; end: number; new_text: string }> | null` — always present on the wire; `null` when no auto-fix edits exist

**PR #335 (J2) reversed the prior `?:` shape.** The `?` was removed because backends always emit all three keys (serde serializes `Option::None` as JSON `null`), so `undefined` was never reachable at runtime. Guards must check `!== null`, NOT `!== undefined` — a `!== undefined` guard silently passes `null` through.

Consumer fixtures pin this invariant: `consumer-node.ts` carries `fix_edits: null` literals and three `@ts-expect-error` missing-key positive controls for `help`, `span`, and `fix_edits`; `consumer-browser.ts` carries the same for `fix_edits` (testing-03 revised). If any of the three fields regresses to optional (`?:`), tsc reports "Unused '@ts-expect-error' directive" and the type-check build fails.

### Node.js and browser exports

Both entry points use `public-types.ts` for the shared exports. This list includes `InitOptions` and `LintFileOptions`. The browser entry uses `LintFileOptions` for `lintVirtual`, which does not read files. Only the Node.js entry exports `CompileFileOptions` and `CheckFileOptions`. `export-surface.spec.mjs` checks both export lists.

Both entries share `CompileOptions`, `CheckOptions`, and `LintOptions`, with `basePath` in each type. The WASM backend rejects a non-null `basePath` in these options at runtime with `mds::invalid_options`.

## Backend Layers

### NapiAddon interface (native.ts)

Covers all seven method names including `lint`, `lintFile`, `lintVirtual`. The lint file surface uses `NapiLintFileOpts = { vars?, rules? }` — no `basePath`. The lint string surface uses `NapiLintOpts = { basePath?, vars?, rules? }`.

### WasmModule interface (wasm.ts)

Exports `compile`, `check`, `lint`, `lintVirtual`, `scanImportRecords`, `preflightModule`, `preflightData`. `lintFile` is NOT in `WasmModule` — file operations are added via `wrapWithFileOps` in `node.ts`, which reads modules through `buildModulesMap(path, wasmModule, readData)` and then calls `wasmModule.compile`/`check`/`lint` directly with a `modules`/`moduleAliases` option. Only `compileFile` passes `readData = true` (KB-INV-5), and it rethrows a compile error as `restoreDataRefusal(err, dataRefusals)` (KB-INV-4).

`compile`/`check`/`lint` on `WasmModule` all accept `moduleAliases?: Record<string, string>` — the keys imports reach a module by other than its own on-disk key (case variants, symlinked-directory spellings). `fileOpts()` and the `lintFile` path in `node.ts` only include this key when `aliases` is non-empty.

`ImportRecord` (the shape `scanImportRecords` returns) has `kind: 'extends' | 'frontmatter' | 'import' | 'export-from' | 'data'`. It is declared in `backend/wasm.ts` beside `WasmModule` — not in `module-scanner.ts`, which only `import type`s it. `scanImports` (the older, path-only export) is **removed** from both `WASM_EXPORTS` and the `WasmModule` interface; the Rust export and the `mds-wasm` README entry still exist (the engine exports more than `WasmModule` requires), just unused by this package now.

`lintVirtual` on the WASM backend deliberately OMITS the `basePath` guard — `LintFileOptions.basePath` is `never` and the public wrapper's `BASEPATH_REJECTORS` rejects it first. That omission follows the project decision that the Node and browser entries share one option type set and that the WASM backend refuses `basePath` at runtime, loudly, with `mds::invalid_options`, instead of with narrowed browser types. Do NOT "fix" it (KB-AP-7).

### Defense in depth across backends

`createNativeBackend` and `createWasmBackend`'s `wrapWithFileOps` each carry per-method `basePath` guards in addition to the public wrapper's (an alternate path must go through the same check as the primary path, or it bypasses it), covering `compileFile`, `checkFile`, `lintFile`, and `lintVirtual`. An internal caller that obtains a backend directly and bypasses the public wrapper's `BASEPATH_REJECTORS` is caught here. Without these backend-level guards, `forwardOpts` would silently drop `basePath` on native (since it's absent from `METHOD_KEYS` for those surfaces) while WASM would throw — producing asymmetric behavior on the same call.

### Conversion of `vars` and `rules` (#479, #483)

Both backends refuse every BigInt in `vars` and `rules` with `mds::invalid_options` and `invalid options.<field>: InvalidArg, typeof BigInt value could not be deserialized`. Native walks the value before it converts it (`reject_unreadable_values` in `crates/mds-napi/src/lib.rs`). WASM copies the value once in a JS helper (`plain_copy`, an `inline_js` snippet in `crates/mds-wasm/src/lib.rs`), and `serde_wasm_bindgen` reads only that copy. The helper checks in the native order: a BigInt, a typed array or ArrayBuffer (skipped), a nesting of 1024 levels, then 1,000,000 values. No BigInt reaches the compiler as a number on either backend. The WASM message carries the native word `InvalidArg, `.

The differences that remain are named in the CHANGELOG, and `backend-contract.spec.mjs` pins each one:

- **A root BigInt** (`vars: 1n`, `rules: 1n`): each backend keeps its own text. Native checks the type of the option first and gives `options.vars must be a plain object, got unknown`. WASM gives the BigInt text.
- **One read per value on WASM.** A getter that gives a string first and `1n` later compiles as the string on WASM. Native refuses it with `a value changed while it was read and could not be deserialized`. A getter that gives `1n` on its first read is refused on both. The test pins the native result and the WASM read count of 1.
- **A read that throws** (a getter or a Proxy trap): WASM gives `mds::invalid_options` with `a value could not be read`. Native passes the exception of the caller through, with no code.
- **A `Map`** is an empty object on both backends. A `Set` or another object that is not an array is copied by its own keys in the same way, but only the `Map` case has a parity test.
- **A `DataView`**: native refuses it as a byte array. WASM copies it as an object with no keys, so `vars` compiles and `rules` gives `options.rules["bad"] must be a severity string, got object`.
- **An inherited enumerable key** (`Object.create({ bad: 1n })`): native refuses the BigInt. WASM reads own enumerable keys only (`Object.keys`) and does not see it.
- **Typed arrays and ArrayBuffers** (a `BigInt64Array` and a typed array with an own `Symbol.iterator` included): the helper passes them on uncopied, and both backends refuse them with `mds::invalid_options` as a byte array or an invalid type. Only the text after the prefix differs. The iterator runs 0 times on each backend.

The copy sets each key with `Object.defineProperty`, never with `copy[key] =`. So an own `__proto__` key stays a key of the copy, and a plain assignment would set the prototype of the copy instead.

### Node floor of the WASM backend (#483)

The `engines` floor of the JS packages is `>=22.12.0`. `@mdscript/mds-napi` keeps `>=22.0.0`, because the native addon does not load the snippet. The nodejs glue of `mds-wasm` `require`s the snippet, and the snippet is an ES module. Node 22.0 to 22.6 fail at load with `Unexpected token 'export'`. Node 22.12.0 is the first Node 22 release where `require` of an ES module is on by default. CI uses the newest Node 22, so CI cannot show a failure below the floor. The snippet ships in `pkg/`, `pkg-web/` and the packed `dist/node` and `dist/web` folders of `@mdscript/mds-wasm`.

## WASM-backend File Pre-scanner (`util/module-scanner.ts`, `util/path-chars.ts`, `util/sliding-window.ts`)

The scanner applies the native path rules in TypeScript, with the differences listed below. The differential tests compare error codes, messages, `help`, and spans where they apply. `ScannerEngine = Pick<WasmModule, 'scanImportRecords' | 'preflightModule' | 'preflightData'>` lists the three engine calls the scanner uses.

### Engine calls

- **`preflightModule(bytes, display, typed)`** wraps the core's `check_module_bytes` (per-file cap, then UTF-8 decode, BOM kept) then `check_module_type` (`not_mds`, judged on `display`'s extension, named in the error by `typed`) — the same order and the same checks `NativeFs::read` makes on every file. `readModule` calls it with `display = keyOf(projectRoot, resolved)` (the module's on-disk-spelled, root-relative key) and `shown` (the path as written) as `typed`.
- **`scanImportRecords(source, asBase)`** returns `ImportRecord[]` — `{ path, kind: 'extends'|'frontmatter'|'import'|'export-from', frontmatterIndex: number|null, span: MdsErrorSpan|null }` — in the order the native resolver resolves them for the way the module is reached: as another module's `@extends` base (`asBase = true`, its own `@extends` listed *after* its imports, mirroring how the resolver walks a base) or for itself (`asBase = false`, matching `scan_imports`' own order). The walk passes `asBase` down from `record.kind === 'extends'` on the *parent's* record, so a base three levels deep still gets the right order. Re-export records (`kind: 'export-from'`) now carry the importer's directive span, as `@import` and `@extends` records do (#423). `data` records come last, in declaration order, once per path, with no frontmatter index or span (the Rust binding filtered them out before #405). A module whose `data:` declarations the engine refuses (too many entries or references, an unknown form) has no data record, so the compile refuses it before any data read, as native does.
- **`preflightData(bytes, display)`**: the per-file cap and UTF-8 check of `check_module_bytes`, no file type check. The caller bounds the bytes.

### Deterministic order despite concurrent reads

The scanner can open at most `MAX_CONCURRENT_OPENS` (16) read-ahead files plus one walk file. It uses `slidingWindow` to start up to `MAX_IMPORTS_READ_AHEAD` (32) reads per module ahead of the walk. The walk checks modules depth first, in the order `scanImportRecords` gives. It reports the first scanner refusal, not necessarily the first native error (U-SM37 and U-SM38, difference 7).

`slidingWindow(items, size, start)` yields one item and refills only when the walk asks for the next item. It keeps up to `size` `start()` calls ahead of the most recent item (U-SW1–U-SW4). If `size` is not a positive integer, it throws `RangeError`.

Each import walked starts a `readAhead()` for it via `startReadAhead`, tracked in a `pending` Set<Promise> so a walk that throws can wait for every outstanding read-ahead to settle before rethrowing (none holds a file open or calls into the engine after the walk finishes — `walkFinished` flag plus `Promise.allSettled(pending)` in the `finally`). `readAheadReads` (a `Map<key, Promise>`) deduplicates: at most one read-ahead per module key, whichever import reaches it first; a module the walk itself reaches later reuses that settled read (`readAheadRead(key)`) instead of reading it twice.

### Byte-charged read-ahead budget

`ReadBudget` (`admit(size): boolean`, `charge(bytes): void`) gates how much the read-ahead may pull into memory before the walk gets there: each module is *admitted* by its `fstat` size against `maxAggregateSize` (10 MiB default), then *charged* the true bytes read once the read completes — a file can grow between its fstat and its read, so the charge can exceed the admission (bounded by `MAX_FILE_SIZE + 1`, the per-file cap-plus-one that `readAtMost` enforces). A module the budget declines to admit ahead of time (`admit` returns `false`) is read later by the walk itself, uncounted against the read-ahead budget — the walk's own `aggregateSize` accumulator (checked AFTER `preflightModule`, never before) is the one guard that can refuse it, and only once every native check on that module has already passed (difference 2 below).

### Error construction and context

- **`pathError(code, message)`** builds the base shape; `PATH_ERROR_HELP` attaches `help` only for `mds::file_not_found` (`'check the file path and ensure the file exists'`) — every other code carries none, on either backend. A `PathError`'s own enumerable keys are exactly `code` (+ `help` when file-not-found, + `span` only once `withImportContext` attaches one) — pinned by U-SM49; do NOT unconditionally set `err.help` for every code.
- **`refusalParts`, a `WeakMap<Error, RefusalParts>`**, holds the detail or `shown` path used to build each scanner `importError`/`fileNotFoundError`. `withImportContext(err, record)` uses it instead of a message-text parse. A frontmatter `mds::file_not_found` becomes `mds::import` with `(in frontmatter imports[<i>])`; an `mds::import` gains the suffix only once. A missing `@import`, `@extends` or `@export … from` target gains its directive span, unless an inner span already exists.
- **Engine-error passthrough has one exception (#423).** A spanless `preflightModule` `mds::not_mds` refusal gains its immediate importer's directive span through `withImportContext`. The helper clones the error and keeps its code, message and help. The `notMdsContextSeen` WeakSet keeps a frontmatter not-MDS error spanless through outer imports. Entry errors stay spanless. Other engine errors pass through unchanged.
- **`nativeParentAndName(path)`** mirrors Rust `Path::parent`/`Path::file_name`: repeated/trailing separators and `.` components don't count, `..` is kept as written (the OS applies it during canonicalization — after symlinks on POSIX, lexically on Windows). `locateEntry` and `locateImport` use that parent and name to resolve a named file. A final `/.` does not change the file.

### Filesystem checks

- **Symlinks**: `assertFileNotSymlink` + `canonicalFile` mirror `NativeFs::check_symlink_named` — refuse by the final component's OWN file type (`openNoFollow`'s `O_NOFOLLOW` → `ELOOP`, or the post-open `lstat` where `O_NOFOLLOW` is unavailable, i.e. Windows, which also reports junctions), never by comparing the canonical path with the path as written (a case-mismatched name on a case-insensitive volume is NOT a symlink, #408).
- **Non-UTF-8**: `resolvePath` gets raw bytes from `realpath(path, { encoding: 'buffer' })`. It refuses an invalid UTF-8 path with `mds::io`, `resolved path is not valid UTF-8: "<path as written>"`. A lossy decode can name a different file (#414).
- **Non-regular file**: `readModule` uses `lstat` before the open and checks the open file with `stat()`. Both checks use `mds::io`, `cannot read <root-relative path>: not a regular file` (#428).
- **Containment and keys**: `locateImport` checks the lexical path before it resolves the canonical directory. It then checks containment on the canonical path. On POSIX, the system applies `..` after the link. Windows collapses `link\..` lexically on both backends (#432).
- **File identity (KB-AP-16)**: `lookupFile` keeps the `lstat` device and inode (`FileIdentity`, exact `bigint`) in `Located.id`. After the open, `readFile` compares them with `handle.stat({ bigint: true })`, before the size check and any read: a directory on the path can change and change back around the open, so path checks alone can pass for another file. A mismatch gives the NativeFs text `mds::io`, `cannot read <root-relative path>: the file changed while it was opened` (`fileChangedError`), for modules and data files (U-SM65 and a data test). On Windows the check differs from native (difference 12).
- **Bounded reads**: `readAtMost(handle, limit, sizeHint)` grows a buffer by doubling (floor `MIN_READ_GROWTH` = 64 KiB) up to `limit` (`MAX_FILE_SIZE + 1`), so a file that grows while being read is never held whole and never more than one byte past the cap reaches the engine (#428).

### Module identity and aliases

`Located.key` is the module's canonical path below the project root, with its on-disk name. The scanner uses that key for reads and for the `walking` and `completed` sets. The WASM engine uses it for dependencies, source-map names, and cycle messages. `Located.aliasKey` is the lexical key before the engine applies aliases.

`checkAndRecordAlias` uses `aliasRefusal` to check new and repeat visits. An alias cannot use a module key or name two module keys. If the lexical key differs from the canonical key, the scanner checks both alias limits before it adds a new alias (difference 8). An identical alias adds no count or bytes. The file APIs pass `BuildModulesMapResult.aliases` to the engine as `moduleAliases`.

`maxModules` permits 256 distinct modules besides the entry by default (#427). The `admitted` count includes the entry, each candidate, and modules whose resolution has not completed; repeated imports consume no extra slot. Native and the scanner share `mds::resource_limit` and `resource limit exceeded: module count exceeds maximum of 256 modules besides the entry`. Binding virtual-map guards remain separate: at most 257 entries, with their own message for 258. A string entry need not occupy a map slot, but the resolved graph still has the same bound.

### Local data pre-scan (#405: `walkData`, `locateData`, `dataRefusals`)

`buildModulesMap(entryPath, engine, readData, options?)`: only `readData === true` reads data, so a JS caller that leaves it out reads none; with `false`, `walk` drops `data` records before it locates them (KB-INV-5). Otherwise `walk` calls `walkData` for each `data` record, after the module's imports. A data file is a leaf, never read as a module or scanned. It shares the sliding window and read-ahead budget, but has its own read-ahead map, `dataReadAheads` (KB-INV-6), and its own `ReadRule`: an `O_NONBLOCK` open (a FIFO put in its place is refused by the handle check, never waited on) and `preflightData`.

**Path rules, in native order** (`locateData`): the string rule as written (`dataPathRefusal`: import path rules, the `./` or `../` prefix, no `{{`, the exact `.json`/`.yaml`/`.yml` extension, no segment that starts with `.` except `.` and `..`, no `node_modules` segment in any ASCII case), then the module import rules (`locateImport`), then the segment rule on the canonical key (Rust `check_key`). Refusals use native's `mds::import`, `data path "<shown>": <reason>`. Rust and the TS tests share `crates/mds-core/tests/fixtures/data-path-vectors.json` (`portable` and `windows` rows).

**Portable rule vs the Windows trim (KB-AP-15).** On Windows, native `check_segments` drops a trailing dot or space from a segment before its `node_modules` test (`TRIMS_SEGMENTS`, `sep === '\\'`); the wasm32 engine never trims. `dataPathRefusal` and the canonical-key check use the trim of this OS, as native does. `dataAliasKey` uses the portable rule (`dataStringRefusal(path, false)`), then `normalizeVirtualKey`, and returns `undefined` only where the engine refuses the path before it computes a key, with native's error; the scan then keeps nothing. A path that only the trim refuses (`./node_modules./a.json`) has an engine key, so the scan keeps native's refusal under it.

**Kept refusals (KB-INV-4).** `walkData` keeps the refusal of a new data key, with its context, in `dataRefusals` under `dataFileNotFoundMessage(aliasKey)`: the exact engine message for a key absent from the map (`data file '<key>' is not in the virtual module map`). The file stays out of `modules`. Native reads only the files a compile binds (not those of an entry that a runtime variable replays or a child entry replaces), and the engine reports the absence at that same point. In `node.ts`, `restoreDataRefusal(err, refusals)` maps a spanless `mds::module_not_found` or `mds::import` with that message to the kept refusal; other errors pass through. The engine checks a data key with no project root before the lookup (`rootlessDataKeyRefusal`, Rust `check_key` on VirtualFs: a leading `/` or `\`, a second byte `:`, a `\` split with no trim), so `walkData` also keeps the refusal under that engine message; the first refusal kept under a message stays.

**Refused-key keep-out (KB-AP-12).** A key with a kept refusal never enters `modules`. A path whose lexical key has a kept refusal returns at once: the engine gives back the first refusal. A later path whose *canonical* key is that key (`./missing/../a.json`, then `./lnk/a.json` through a link `lnk` to `.`, or a case variant on a case-insensitive volume) throws `aliasConflictError` inside the `try`, so the later path gets the alias conflict as its kept refusal. A path whose key is already in `dataKeys` or `aliases` goes through `checkAndRecordAlias`, and its refusal is thrown at once with the data context, because the engine cannot report it (KB-INV-8).

**Separate data context for `@extends` (KB-AP-13).** `walk` takes `context` for import refusals and `dataContext` for data refusals. A child gets `childContext` for both, but an `@extends` base gets its child's `dataContext`: native binds base data in the child's scope, outside the `@extends` span, so the error has no `@extends` context. A refusal that `walkData` throws with full context goes into the `dataRefusalsInContext` WeakSet, and `walk` rethrows it with no more `withImportContext`. U-WCF15 pins one base, a chain of two and an import of the child, with `child.mds` as the control.

**Canonical key with no data extension.** A case-insensitive volume can spell `./cfg.json` as `CFG.JSON`. The scan does not read it: it records the alias and puts an empty text under the key (`modules[key] ??= ''`), and the engine refuses it as native does (`mds::data`, `resolved data file must have a .json, .yaml or .yml extension`) before it reads the text. The empty text counts in the engine's module-key bound (difference 10).

**Data bounds (KB-INV-7).** `MAX_DATA_FILES` and `MAX_DATA_BYTES` mirror the `mds::` constants of the same names, counted apart from modules, with the NativeFs texts (`dataFileCountError`, `dataBytesError`, `mds::resource_limit`). The count check runs before the read of a new key, the byte check after it. The first bound reached sets `dataBound`; then `walkData` reads no new data file and `readAhead` starts no new data read. A read ahead started before then can still finish a data file past the bound, within the window and budget; the walk uses none of them. Bound refusals are kept, not thrown.

### Backend differences (see `buildModulesMap`)

1. WASM can report an escape for a missing module outside the project. It checks containment before it checks the file (U-SM30, U-SM32).
2. Only WASM has an aggregate-size guard (`maxAggregateSize`). The guard never pre-empts a refusal of the same module (U-SM39).
3. WASM refuses each lexical `..` above the key root before disk resolution. This includes `/..`, which native resolves to `/` (#424; U-S16, U-SM34). It also includes `link/..` above the project root as written (U-SM52).
4. On POSIX, a `link/..` import can use an alias. On Windows, both backends apply `..` lexically for disk access. The WASM key splits only at `/`, so `./link\..\z.mds` and `./a\b/../z.mds` can use aliases too. WASM refuses to use one module key for two files (#432; U-SM51 to U-SM55, U-SM58 to U-SM60).
5. WASM limits the entry key, resolved import key, and canonical alias target to 256 segments (`mds::resource_limit`). The alias-target check never pre-empts a refusal of that module itself (U-S16, U-SM56).
6. A `cannot read` message can use an errno name instead of Rust's system error text. Node's file error message can show an absolute path. In Node 22.12.0 and later, `util.getSystemErrorMessage(errno)` gives libuv text without a path. It gives `permission denied` for `EACCES`, for example, but Rust gives `Permission denied (os error 13)`. Both backends show the path below the project root, or `.` for the root (U-SM28, U-SM31, U-SM33, U-SM57). For a POSIX name with a backslash, native displays `/` and the scanner keeps the backslash (U-SM62).
7. The scanner reads modules before the engine evaluates them. A later import refusal can precede an earlier compile error or cycle (U-SM38).
8. The scanner limits distinct aliases to 65,536 entries and 10 MiB of UTF-8 keys plus targets. It checks both limits before it adds a new entry (U-SM63, U-SM64). The engine checks the same limits. Native does not use an alias map.
9. WASM refuses a non-UTF-8 canonical directory before it checks the file below it. Native can report a missing file, final symlink, escape, or forbidden character first. To match native order, each path check would need string and byte forms. A check on one form does not prove the other (U-SM48, Linux CI).
10. The scan reads every data file a module lists, also those of an entry that a runtime variable replays or a child entry replaces, which native skips; they count toward the data bounds, so WASM can refuse where native passes. Empty texts of keys with no data extension count in the engine's module-key bound. A later path whose canonical key is a refused path's key gets `mds::import` (alias conflict) where the compile binds it and not the refused path. Deliberate: the scan knows no runtime variable or entry name. Differences 1, 3, 6 and 9 apply to data files too.
11. On POSIX, WASM refuses a data file named with a leading `\` or a second byte `:` (`\x.json`, `a:b.json` in the root), which native reads: `mds::import`, `data path "<path>": escapes project directory`. The engine checks data keys with no project root (`rootlessDataKeyRefusal`), and no alias helps when the key is the file's own canonical key (the `known limit` test).
12. On Windows, the scanner compares the volume serial number and file index of the opened and the located file; native compares the final path of the handle. A regular file put at the path between lookup and open is refused as changed on WASM, and native reads it. Deliberate: Node gives no final path of a handle, and the stricter check never reads another file.

### Tests

`__test__/forbidden-path-chars.spec.mjs` (U-FP1–U-FP5) compares Rust and JavaScript for all 80 forbidden codepoints. `__test__/scanner.spec.mjs` supplies most parity tests:

- U-S11–U-S16 check `normalizeVirtualKey`. U-S17 checks the alias guard.
- U-SM3, U-SM7, and U-SM9–U-SM37 cover path and I/O checks. U-SM34 needs a root container.
- U-SM38 and U-SM39 check error order. U-SM40–U-SM42 check import context.
- U-SM43 and U-SM46 check file growth. U-SM44 checks open files after a refusal. U-SM45 and U-SM47 check duplicate calls.
- U-SM48 and U-SM48b check non-UTF-8 directories on Linux. U-SM49 checks error fields. U-SM50 checks `WALK_FINISHED`.
- U-SM51–U-SM55 check alias conflicts, containment, the canonical target, import kinds, check and lint, and the raw engine guard.
- U-SM56 checks the segment limit. U-SM57 checks file-handle errors. U-SW1–U-SW4 check `slidingWindow`.
- U-SM58–U-SM60 check Windows aliases and containment. U-SM61 and U-SM62 check POSIX backslash paths. U-SM63 and U-SM64 check both scanner alias limits. U-SM65 checks the file identity of an opened module.

`__test__/data-scanner.spec.mjs` holds the data pre-scanner differentials (TP-20, TP-21, TP-23, TP-26, TP-27, TP-56, TP-112 and named tests): every `portable` row of `data-path-vectors.json` on both backends and through the TS scanner; on Windows only, the `windows` rows plus trailing-space, `\`-separator and control rows (KB-AP-14); links, FIFOs, the changed-file refusal, the read-ahead window, two files under one key, the refused-key keep-out, the rootless key, the POSIX known limit, both bounds and the key with no data extension. Each refusal test asserts that no byte of a refused file reaches `preflightData`, with a control that reads a file. `data.spec.mjs` has the public-API arms of TP-15, TP-19 and TP-52. In `bundler-utils`, `data-deps.spec.mjs` (TP-64) checks that the adapters watch data files: `dependencies` lists modules in first-resolution order, then data files in first-read order.

`__test__/wasm-compileFile.spec.mjs` U-WCF12–U-WCF14 compare dependencies, source-map names, and cycle text. U-WCF15 compares data reads and refusals with native, after it pins the native outcomes (so the equality is not of two failures). The WASM backend loads `crates/mds-wasm/pkg/` first. After a Rust change, rebuild it from the current tree before the tests.

The new `backend-contract.spec.mjs` tests compare live conversion errors (BigInt, depth and count refusals in `vars` and `rules` included), importer spans, spanless nested frontmatter errors, and flat/nested graphs at 255–259 total modules. Both backends accept 255–257 and refuse 258–259 with the shared code and message. These tests fail if a required engine is absent; they do not skip locally. `scanner.spec.mjs` U-SM5 covers distinct and in-flight counts. Its shared-message test reads the reference from the live WASM core. U-WCF13 now pins the importer's not-MDS span and keeps entry errors spanless.

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
  'preflightData',
] as const;
```

`BASE_METHODS` now includes `lint` (base backends share it), and `NODE_METHODS` now includes `lintFile`. `WASM_EXPORTS` no longer lists the old path-only `scanImports` export — it requires `scanImportRecords`, `preflightModule` and `preflightData` instead, the three calls the file pre-scanner actually makes (U-BC3 and U-WB17 pin the list); the WASM module MAY export more (e.g. the engine's own `scanImports` still exists), but only the names in `WASM_EXPORTS` are required by `validateBackendMethods`.

`createNativeBackend` validates `[...BASE_METHODS, ...NODE_METHODS]` plus `lintVirtual`. Missing method throws a plain `Error` (not `mds::` coded) naming the missing method.

## assertResultShape — O(1) shallow validator

- `kind='compile'`: branches on `result.kind`; asserts `output` is string or `messages` is array; asserts the **inactive field is absent**; asserts `warnings`/`dependencies` are arrays
- `kind='check'`: only asserts `warnings` is array
- `kind='lint'`: asserts `files` is array, `truncated` is boolean, `version` is number
- PERF-04 constraint: uses only `Array.isArray()` — never accesses array elements. A Proxy must observe zero numeric-index reads.
- Throws `Error` with `code: 'mds::invalid_backend_result'` on shape violation

## Anti-Patterns

- **KB-AP-1 Re-introducing per-surface builder functions** — `varsOpt`/`compileOpt`/`lintOpt`/`lintFileOpt` were deleted because they each hardcoded their own key arrays, which drifted from `METHOD_KEYS` without a compile error. This was #180: an alternate path that skipped a check only the primary path made.
- **KB-AP-2 Hardcoding basePath rejection in only some of the four file-surface methods** — the F-03 regression. All four must use `BASEPATH_REJECTORS` with purpose-built errors and identical napi messages.
- **KB-AP-3 Writing a basePath rejection test with `.catch()` or `assert.rejects`** — synchronous throws escape both; must use `assert.throws`.
- **KB-AP-4 Accessing `result.output` without branching on `result.kind`** — TypeScript will catch this at compile time since `CompileResult` is a union.
- **KB-AP-5 Including `compileMessages` or `scanImports` in `WASM_EXPORTS` or `BASE_METHODS`** — `compileMessages` was deleted from the WASM binding; `scanImports` was replaced by `scanImportRecords` (#414) and is no longer required.
- **KB-AP-6 Returning `{ kind: 'markdown', output: ..., messages: [] }` from mocks** — inactive field must be ABSENT; `assertResultShape` rejects it.
- **KB-AP-7 Adding a basePath guard to WASM `lintVirtual`** — the public wrapper fires first; the guard in `lintVirtual` is deliberately absent, because the shared option types leave the `basePath` refusal to the runtime wrapper.
- **KB-AP-8 Reordering METHOD_KEYS witness literals** — key order determines the `recognised keys are: …` list; U-OV-14/U-OV-31 pins it via `strictEqual` against live napi, not a hardcoded string.
- **KB-AP-9 Unconditionally setting `err.help` on every `PathError`** — only `mds::file_not_found` carries one on either backend; U-SM49 asserts the exact own-key set per refusal shape.
- **KB-AP-10 Comparing a canonical path with the path as written to detect a symlink** — misreports a case-mismatched spelling on a case-insensitive volume as a symlink (#408). Judge the final component's own file type instead (`O_NOFOLLOW`/`lstat`), and compare canonical with canonical only.
- **KB-AP-11 Reading a module's full imports without a bound on concurrency or read-ahead** — a module with thousands of imports or data entries would hold thousands of pending file descriptors; `MAX_CONCURRENT_OPENS`/`MAX_IMPORTS_READ_AHEAD`/`slidingWindow` exist specifically to cap this. Data records use the same window (TP-27).
- **KB-AP-12 Keeping a refusal under one key while another path can fill that key** — if a later path's file enters `modules` under the lexical key of a refused path, the engine binds that file to the refused path and raises no error for `restoreDataRefusal` to map: the compile passes where native fails. Also check case variants (commit `e8fad20`). See "Refused-key keep-out".
- **KB-AP-13 Using the import context for the data refusals of an `@extends` base** — the message and span then differ from native, also through a chain of bases. See "Separate data context for `@extends`".
- **KB-AP-14 Running only the portable rows of a shared vector table on a platform where the surfaces differ** — on Windows, native trims segments and the wasm32 engine does not, so the `windows` rows of `data-path-vectors.json` are the ones that can fail there. A suite of portable rows only passes on Windows and proves nothing. Run the platform rows on their platform with a control row that reads a file, and skip them elsewhere with a reason.
- **KB-AP-15 Keying a data path with the Windows segment trim** — a path that only native's trim refuses then gets no key and no kept refusal; the engine looks up its key, and the compile shows the raw `is not in the virtual module map` error instead of native's `node_modules segments are not allowed`.
- **KB-AP-16 Trusting path checks to name the opened file** — `lstat`, `realpath` and the post-open checks inspect the path, not the handle; a directory swapped and swapped back around the open passes them while the handle holds another file. Validate and open the same representation, and never reach a file through a key that a lossy step made. See "File identity".

## Gotchas

- **KB-AP-3, KB-INV-1: `lintFile`/`lintVirtual` on the file surface throw for basePath — use `assert.throws`, not `assert.rejects`** — this is the same synchronous-throw channel as `compileFile`/`checkFile`.
- **Type fixtures use `dist/`.** The fixtures import `dist/node.js` or `dist/browser.js` to check the built declarations. Build the package before a direct fixture check. The package's `npm test` runs the `pretest` build, and `prebuild` clears `dist/`.
- **Run `tsc` through the workspace.** Use `npm run build -w <pkg>` to build with the workspace compiler. Use `npm test -w @mdscript/mds` to rebuild the package and check both `exactOptionalPropertyTypes` modes. For one config, build the package first. Then run `npm exec -w <pkg> -- tsc -p <workspace-relative-config>`. The TypeScript workspaces use 7.0.2, but `npx tsc` at the repo root runs 6.0.3.
- **The `pretest` build has limits.** It does not run for `test:native`, `test:perf`, or a direct `node --test` call. It does not rebuild the native `.node` addon or WASM `pkg/`. In a root `npm test --workspaces` run, the `bundler-utils` tests run before the mds `pretest`.
- **File option types reject `{ basePath: '/' }`.** The `basePath?: never` field rejects a string with EOPT on or off. `consumer-node.ts` checks this case.
- **Missing-engine checks differ by test.** Some older differentials warn-and-return locally but fail under `process.env.CI`. New conversion, importer-span and module-count tests require live engines and fail locally too. Rebuild the native addon and `crates/mds-wasm/pkg/` before those tests.
- **`CheckResult` has only `{ warnings: string[] }` — no `dependencies`** — this matches the napi wire; check does not expose deps.
- **The `assertReady()` error message is byte-literal** — `'@mdscript/mds: call await init() before using compile/check/compileFile/checkFile/getBackend'` — tests pin it; don't change without updating tests.
- **`basePath: undefined` is treated as "key absent"** — `getBasePathError` uses `!== undefined`, not `!= null`. U-OV-29 requires both backends to accept `basePath: undefined` on all seven methods. This is consistent with `forwardOpts`'s `!= null` filter that prevents `{basePath: undefined}` from reaching napi.
- **Option key lists live in four independent places** (TS `METHOD_KEYS`, napi, wasm, Python). TS↔napi parity is runtime-enforced; WASM and Python drift is caught only by prose review until #311 lands.
- **`cannot read` names the path below the project root.** Compare the display path and the code across backends. The reason for a Node error can differ (difference 6).
- **`U-SM34` needs a root container.** A non-root local run does not test that case.
- **`U-SM48` and `U-SM48b` use Linux byte names.** macOS and Windows skip these fixtures because their filesystems cannot create such a name.
- **A path with `..` can give an alias on POSIX or Windows.** On POSIX, for a path with `/` separators such as `./link/../z.mds`, disk resolution follows the link before it applies `..`. On Windows, the scanner can also record an alias for a backslash path such as `./link\..\z.mds`. The scanner splits the path at `/` only, so the backslashes stay in its virtual key (difference 4).
- **KB-INV-10: An alias can name a file other than its lexical key.** The walk must see every import that the engine resolves. The Rust test `scan_import_records_classifies_each_import_form` requires a classification for new syntax variants. Update `scan_import_records` and the walk together for a new import form.
- **A directory, FIFO, device, or socket gets `not a regular file`.** Both backends use `mds::io` for this case (#428).
- **KB-INV-4: `buildModulesMap` does not throw for a refused data file.** It resolves; look in `dataRefusals` and check that no byte reached `preflightData`, or call `compileFile`. A refused data file of an entry that a child entry or a runtime variable replaces compiles with no error, as on native.
- **KB-INV-4: The restore key is engine text.** `dataFileNotFoundMessage` and `rootlessDataKeyRefusal` copy Rust messages byte for byte. If Rust changes either text, `restoreDataRefusal` misses and WASM shows the raw engine error; only the differentials catch it.
- **KB-INV-7: A read-ahead can read a data file past a bound.** A test that counts `preflightData` calls must allow reads that started before the bound. The byte-bound test asserts the data keys in `modules` and the kept refusals, not the reads.
- **KB-INV-3: A stale `pkg/` hides data records.** Before #405 the Rust binding filtered them out; with an old `crates/mds-wasm/pkg/`, the scan reads no data and the data tests fail. Rebuild `pkg/` after a pull.

## Key Files

- `packages/mds/src/types.ts` — option types such as `CompileFileOptions`, result types, and internal backend interfaces
- `packages/mds/src/public-types.ts` — shared type and value exports for both entries
- `packages/mds/__test__/export-surface.spec.mjs` — source export lists and runtime value exports
- `packages/mds/src/util/options.ts` — `METHOD_KEYS`, `forwardOpts`, `assertKnownKeys` (now WIRE-escapes unknown keys), `getBasePathError`, `BASEPATH_REJECTORS`, `MethodName`
- `packages/mds/src/backend/contract.ts` — `BASE_METHODS`, `NODE_METHODS`, `WASM_EXPORTS`, `ResultKind`, `assertResultShape`, `validateBackendMethods`
- `packages/mds/src/backend/native.ts` — `NapiAddon` interface, `createNativeBackend` (with per-method depth-in-defense basePath guards)
- `packages/mds/src/backend/wasm.ts` — `WasmModule` interface, `ImportRecord`, `createWasmBackend`, `fileOpts`, WASM basePath error
- `packages/mds/src/node.ts` — `wrapWithFileOps`, all seven public functions, type re-exports
- `packages/mds/src/browser.ts` — browser-safe re-exports; no file operations
- `packages/mds/src/util/module-scanner.ts` — `buildModulesMap`, `ScannerEngine`, `normalizeVirtualKey`, `PathError`, `findProjectRoot`, `canonicalFile`, `openNoFollow`, `readAtMost`, `cannotReadError`, `readError`, `symlinkError`, `escapesProjectError`, `notRegularFileError`, `closeModule`, `aliasRefusal`, `withImportContext` (WASM-backend file pre-scanner); data pre-scan: `walkData`, `locateData`, `dataPathRefusal`, `dataAliasKey`, `rootlessDataKeyRefusal`, `restoreDataRefusal`, `MAX_DATA_FILES`, `MAX_DATA_BYTES`, `FileIdentity`, `fileChangedError`
- `packages/mds/src/util/path-chars.ts` — JS mirror of the #265 forbidden-path-character rule; `sanitizeControlCharsWire`, `escapePathForMessage`, `isForbiddenPathChar`, `forbiddenCharMessage`, `importPathViolation`
- `packages/mds/src/util/sliding-window.ts` — `slidingWindow`, the read-ahead concurrency generator
- `packages/mds/__test__/options-validation.spec.mjs` — U-OV-1..U-OV-36; option validation, basePath forwarding, synchronous-throw contract, byte-identical message parity
- `packages/mds/__test__/scanner.spec.mjs` — U-S11–16, U-SM3–65, U-SW1–4; the native-vs-WASM pre-scanner differential
- `packages/mds/__test__/data-scanner.spec.mjs` — the native-vs-WASM data pre-scanner differential and the shared data path table
- `packages/mds/__test__/data.spec.mjs` — TP-15, TP-19, TP-52 JS arms: data through the public API on both backends
- `packages/mds/__test__/wasm-compileFile.spec.mjs` — U-WCF1–15; whole-shape compiled-output parity (dependencies, sourceMap.sources, cycle text), not-MDS importer spans and data reads
- `crates/mds-core/tests/fixtures/data-path-vectors.json` — the data path vectors that Rust and the TS tests share (`portable`, `windows`)
- `crates/mds-wasm/src/lib.rs` — the `scanImportRecords` and `preflightData` exports
- `packages/mds/__test__/backend-contract.spec.mjs` — live native/WASM conversion, import-context and module-count comparisons; required engines must be present
- `packages/mds/__test__/forbidden-path-chars.spec.mjs` — U-FP1–U-FP5; the 80-codepoint differential
- `packages/mds/__test__/types/consumer-node.ts` — AC-P3-20/21 type-level matrix for Node entry
- `packages/mds/__test__/types/consumer-browser.ts` — AC-P3-16/20 type-level matrix for browser entry

## Related

Project rules that shape this area, stated in words:

- **Shared option types**: both entries share the string-surface option types; WASM refuses `basePath` at runtime (AC-P3-20: `basePath` is a POSITIVE case in `consumer-browser.ts`).
- **One enforcement point**: an alternate path must not bypass a check of the primary path (backend basePath guards, `forwardOpts`, the aggregate-size guard after native's checks, data path rules before `preflightData`).
- **No vacuous tests**: a test that asserts only absence, or never reaches the guarded path, passes vacuously (U-OV-22, U-OV-14/U-OV-31, U-SM43/46/47 controls, the read control of each data refusal test).
- **Compare surfaces with each other**: goldens per surface prove no parity; the differentials (U-SM21, U-SM32, U-SM37, U-WCF12–15, `data-scanner.spec.mjs`) compare with live native.
- **Census every message site**: the path-escaping fixes (#265, #408) cover every message helper in `module-scanner.ts`/`path-chars.ts`.
- **A bound limits only what it counts, where it runs** (KB-INV-7). **Validate and open the same file** (KB-AP-16).
- **Rebuild `crates/mds-wasm/pkg/`** before a differential; a stale `pkg/` runs an older engine.
- **CI-only checks**: U-SM34 (root) and U-SM48 (non-UTF-8 names, Linux); the `windows` data rows run only on Windows.

Other knowledge bases and files:

- Feature: mds-napi — the native backend; compile/compileFile return the discriminated union this package types
- Feature: mds-lint — the lint surface; `LintResult`, `LintDiagnostic`, `LintFileOptions` types here are the wire contract for mds-napi's lint output
- Feature: bundler-plugins — imports `compileFile` from this package via `MdsApi` in `bundler-utils/src/types.ts`
- Feature: mds-compiler — the Rust `CompileResult`/`CompiledOutput` types, `check_module_bytes`/`check_module_type`, `scan_import_records`, and `VirtualFs::with_aliases` that this package's pre-scanner mirrors; also the local data sources (`read_data`, `data::path`, `MAX_DATA_FILES`, `MAX_DATA_BYTES`) that the data pre-scan mirrors
