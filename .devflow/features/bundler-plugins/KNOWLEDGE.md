---
feature: bundler-plugins
name: MDS Bundler Plugins & Loaders
description: "Use when adding a new bundler integration, modifying the emitted-module contract, debugging HMR behavior, working on the CJS compatibility shim, updating the transformer/loader factory, registering a new package in the release pipeline, or investigating why a .md file with type:mds frontmatter doesn't hot-reload. Keywords: createMdsTransformer, createMdsLoader, bundler-utils, vite-plugin, rollup-plugin, webpack-loader, rspack-loader, addWatchFile, addDependency, handleHotUpdate, canon, transformed Set, HMR_ENABLED, MDS_HMR, new Function CJS shim, esmImport, LazyInit, emitted module contract, export default string, metadata.dependencies, metadata.warnings, shouldTransform, cleanId, isMdsExtension, dual ESM+CJS build, tsconfig.cjs.json, write-cjs-package.cjs, dist-cjs, D1 no HMR self-accept, D3 createMdsLoader factory independent state, D5 Linux-gated HMR e2e, D7 native watcher semantics, G1 fix, canon symmetry, E-norm symlink, ADR-014 deps-before-entry, PF-003 provenance directory field, verify-versions, bump-version, 8-package release gate."
category: architecture
directories:
  - packages/bundler-utils/
  - packages/vite-plugin/
  - packages/rollup-plugin/
  - packages/webpack-loader/
  - packages/rspack-loader/
referencedFiles:
  - packages/bundler-utils/src/transform.ts
  - packages/bundler-utils/src/loader.ts
  - packages/bundler-utils/src/types.ts
  - packages/bundler-utils/src/frontmatter.ts
  - packages/bundler-utils/src/lazy-init.ts
  - packages/vite-plugin/src/index.ts
  - packages/rollup-plugin/src/index.ts
  - packages/webpack-loader/src/index.ts
  - packages/rspack-loader/src/index.ts
  - packages/bundler-utils/__test__/hmr-harness.mjs
created: 2026-06-16
updated: 2026-06-16
---

# MDS Bundler Plugins & Loaders

## Overview

Five packages form the bundler integration layer for MDS: a shared `@mdscript/bundler-utils` library plus four thin wrappers — `@mdscript/vite-plugin`, `@mdscript/rollup-plugin`, `@mdscript/webpack-loader`, and `@mdscript/rspack-loader`. The shared library owns 100% of the compilation logic; each wrapper is fewer than 20 lines.

The critical architectural property is the **emitted-module contract**: every `.mds` (or `.md` with `type: mds` frontmatter) file is transformed into a JS module with the shape `export default "<compiled string>"; export const metadata = { warnings, dependencies };`. No HMR self-accept footer is ever injected (applies D1). This contract is the stable surface that downstream consumer code imports.

## System Context

The compiler call chain is:

```
bundler file pipeline
  → shouldTransform(id)          — frontmatter.ts (sync .mds, async .md peek)
  → transformer.transform(id)   — transform.ts → mds.compileFile() → @mdscript/mds
  → emitted JS module           — export default + metadata
  → bundler error/warning API   — errors.ts formatMdsError()
```

`@mdscript/mds` (the mds-js feature) is the only runtime peer dependency. `bundler-utils` calls `mds.init()` once via `LazyInit`, then `mds.compileFile()` on every transform call.

## Component Architecture

### bundler-utils — shared library

Three logical layers inside `packages/bundler-utils/src/`:

| File | Role |
|------|------|
| `types.ts` | Shared interfaces: `MdsApi`, `CompileResult`, `TransformResult`, `MdsPluginOptions`, `FormattedError` |
| `frontmatter.ts` | `shouldTransform` (sync for `.mds`, async 512-byte peek for `.md`), `cleanId`, `isMdsExtension` |
| `lazy-init.ts` | `LazyInit<T>`: single-init dedup with generation-counter TOCTOU safety |
| `transform.ts` | `createMdsTransformer` — stateful factory wrapping `mds.compileFile` |
| `loader.ts` | `createMdsLoader` — factory for webpack/rspack loader instances |
| `errors.ts` | `formatMdsError` — normalizes MDS compiler errors to `FormattedError` |

### createMdsTransformer — the one shared compilation path

Every bundler plugin creates exactly one transformer via `createMdsTransformer(mds, options)`. The factory:
- Lazily initializes the compiler backend on first `transform()` call
- Calls `mds.compileFile(id, { vars })` to get `{ output, warnings, dependencies }`
- Emits: `export default "<escapeForJs(output)>";\nexport const metadata = <safeJsonForJs({ warnings, dependencies })>;\n`

The two string escaping helpers are non-obvious:

`escapeForJs` handles `\\`, `"`, `\n`, `\r`, `\0`, U+2028, U+2029 — the last two are JS line terminators that `JSON.stringify` does NOT escape and must be escaped explicitly to avoid breaking `export default "..."` string literals.

`safeJsonForJs` escapes `<` (to `<`, prevents `</script>` injection), U+2028, and U+2029 — same reason, but for the `metadata` JSON side.

### createMdsLoader — the webpack/rspack factory (applies D3)

`createMdsLoader()` returns `{ loader, _resetForTesting, _setTransformerForTesting }`. Key semantics:
- Each call creates **independent per-instance state** — its own `LazyInit<Transformer>` and `capturedOptions`. Calling `createMdsLoader()` twice yields non-interfering instances.
- `webpack-loader/src/index.ts` and `rspack-loader/src/index.ts` are identical one-liners: call `createMdsLoader()` once at module scope and re-export the three symbols.
- `_setTransformerForTesting` is intentionally **un-gated** (no `NODE_ENV` check) — applies D3. The matching Vite/Rollup helpers ARE gated on `NODE_ENV=test`.
- Options are captured on the first loader invocation; later calls with different options emit a warning but continue using the original options (webpack semantics).

### CJS compatibility shim

`@mdscript/mds` is ESM-only. Webpack/Rspack resolve loaders with `require()`, so `bundler-utils`, `webpack-loader`, and `rspack-loader` ship a CJS build alongside their ESM build. The critical pattern in `loader.ts`:

The `new Function()` wrapper preserves a native `import()` call through the TypeScript CJS compiler. TypeScript rewrites `import(specifier)` to `require(specifier)` under `"module": "CommonJS"`, breaking ESM-only packages. The wrapper bypasses this by hiding the import inside a string literal the TS compiler cannot see.

```typescript
// loader.ts — module level (not inside createMdsLoader)
const esmImport: () => Promise<unknown> = new Function(
  'return import("@mdscript/mds")',
) as () => Promise<unknown>;
```

Key properties of the shim:
- The specifier `"@mdscript/mds"` is **hardcoded** — no parameter is accepted through the `new Function()` boundary. This eliminates the latent code-loading vector that a parameterized `new Function('id', 'return import(id)')` would create.
- `new Function()` is equivalent to `eval()` for CSP. This is intentional and safe for the Node.js loader context (no browser CSP). The comment in `loader.ts` documents this explicitly.
- The shim is validated at runtime: the result is checked for `typeof .then === 'function'` and the resolved module shape is checked for `compileFile` and `init` functions before use.

The dual build is wired in `package.json` scripts:
```
"build": "tsc -p tsconfig.json && tsc -p tsconfig.cjs.json && node ../../scripts/write-cjs-package.cjs dist-cjs"
```
`tsconfig.cjs.json` targets `"module": "CommonJS"` with `"moduleResolution": "Node10"` and emits to `dist-cjs/`. `write-cjs-package.cjs` writes `dist-cjs/package.json` with `{"type":"commonjs"}` so Node.js treats the files as CJS without requiring `.cjs` extensions.

Only `bundler-utils`, `webpack-loader`, and `rspack-loader` ship dual ESM+CJS builds. `vite-plugin` and `rollup-plugin` are ESM-only (`dist/` only, no `main` field, no `dist-cjs/`).

## Component Interactions

### Vite plugin — handleHotUpdate and the canon() / transformed Set (G1 fix)

Vite's `handleHotUpdate` must detect when a tracked MDS file (or one of its `@import` dependencies) changes and send a full-reload. The challenge is that:
- `.md` files with `type: mds` do not have the `.mds` extension, so `isMdsExtension()` misses them.
- macOS symlinks: `/tmp` is a symlink to `/private/tmp`, so the same physical file can appear under two paths (edge E-norm).
- Vite may pass ids with `?t=123` cache-busting suffixes to `transform()`.

The G1 fix addresses all three with a **closure-level `Set<string>` keyed by `canon(path)`**. The `canon()` function:
1. Calls `cleanId(p)` to strip query/hash fragments.
2. Normalizes OS separators to forward-slash.
3. Calls `realpathSync()` to resolve symlinks; falls back to `path.resolve()` if the file was deleted.

`canon()` is called **both** at insert time (inside `transform()`) and at lookup time (inside `handleHotUpdate()`). Using the same function on both sides guarantees symmetry even across symlinks.

`handleHotUpdate` triggers a full-reload under three conditions:
1. `isMdsExtension(cleanId(ctx.file))` — fast path for `.mds` files (no transform lookup needed).
2. `transformed.has(canon(ctx.file))` — file was previously compiled (covers `.md+type:mds`, deleted files).
3. `ctx.modules?.some(m => m.id != null && transformed.has(canon(m.id)))` — a module in Vite's graph includes a tracked MDS id (transitive dep path); `ctx.modules` may be absent or contain `null` ids.

Returns `[]` (suppress default HMR) when a reload is sent; `undefined` (let Vite handle it) otherwise.

### Rollup plugin — addWatchFile vs addDependency

Rollup uses `this.addWatchFile(dep)` (not `addDependency`). The plugin iterates `result.dependencies` (absolute paths from the MDS compiler) and calls `this.addWatchFile(dep)` so Rollup triggers a full rebuild when any transitive `@import` dependency changes. Rollup has no browser HMR protocol — there is no `handleHotUpdate` equivalent. The plugin does not expose that hook (applies D1).

Error reporting uses `this.error(message, pos)` which throws into Rollup's error display, whereas Vite uses `throw new Error(...)` with `.id` and `.loc` properties attached.

### Webpack and Rspack loaders — addDependency

Webpack/Rspack use `this.addDependency(dep)` (not `addWatchFile`). The loader iterates `result.dependencies` and calls `this.addDependency(dep)`. Webpack's built-in module graph handles the rest — the loader does not inject `module.hot` or `import.meta.webpackHot` (applies D1). Full reloads bubble to the root because there is no self-accept footer (`hot: true` strategy).

## Integration Patterns

### Adding a new bundler integration

For a new bundler `X`:
1. Create `packages/x-loader/src/index.ts` — call `createMdsLoader()` once at module scope, re-export `{ loader, _resetForTesting, _setTransformerForTesting }`.
2. Add `tsconfig.json` (ESM) and `tsconfig.cjs.json` (CJS) if the bundler requires CJS resolution.
3. Add `package.json` with `"repository": { "directory": "packages/x-loader" }` (required for npm provenance — applies PF-003).
4. Register in both `scripts/verify-versions.mjs` and `scripts/bump-version.mjs` (the `PKG_PATHS` array in each) — **this is the 8th-package pattern**: rspack-loader was the 8th entry and both scripts were updated together.
5. For watch-mode: call the bundler's equivalent of `addWatchFile`/`addDependency` for each path in `result.dependencies`. Register `@import` dependency files **before** the entry file (applies ADR-014 deps-before-entry ordering).

### shouldTransform — the `.md` async path

`.mds` files: `shouldTransform()` returns `true` synchronously.

`.md` files: `shouldTransform()` returns a `Promise<boolean>`. It opens the file, reads the first 512 bytes, and checks for YAML frontmatter with `type: mds`. Bundler plugin `transform()` hooks must `await` the result. The 512-byte cap means very large `.md` files are NOT fully read — frontmatter must appear within the first 512 bytes.

## Constraints

- `createMdsLoader()` captures options on the first call. Using a single webpack process with different loader options for different file sets is not supported — separate processes are required.
- The `LazyInit` factory retries on rejection, but options are fixed after the first successful init. Changing vars between builds requires a full process restart.
- HMR e2e specs (`hmr-e2e.spec.mjs`, `watch-e2e.spec.mjs`) are gated to Linux (applies D5): `HMR_ENABLED = process.platform === 'linux' || process.env.MDS_HMR === '1'`. macOS FSEvents has higher latency and does not surface read-access events. Windows uses a different notify backend. Set `MDS_HMR=1` to force-enable locally.
- The transformed Set in the Vite plugin grows with each distinct compiled file and its deps. Stale entries (deleted files) cause at most one extra `handleHotUpdate` check, never unbounded growth. No eviction is needed.
- D7 behavioral limits: delete/recreate, create-after-error, and `.md`→`type:mds` flip follow native bundler watcher semantics — these are documented limits, not parity hacks.

## Anti-Patterns

- **Injecting HMR self-accept code into the emitted module**: The contract is a plain `export default "<string>"; export const metadata = {...};` with no `module.hot`, `import.meta.hot`, or `import.meta.webpackHot`. Webpack/Rspack bubble reloads to root; Vite sends `full-reload` from `handleHotUpdate`. Adding self-accept would create a footgun (`hot:'only'` in webpack/rspack would silently skip the reload).
- **Calling `canon()` on only one side**: If `canon()` is applied at insert but not at lookup (or vice versa), symlink paths and query-suffixed ids will fail to match. The G1 fix requires calling `canon()` identically in both `transform()` (insert) and `handleHotUpdate()` (lookup).
- **Adding a `NODE_ENV=test` gate to `_setTransformerForTesting` in the loader**: The webpack/rspack version is intentionally un-gated (applies D3). Adding a gate breaks webpack/rspack test suites. Use `_setTransformerForTesting(null)` in `afterEach` to clean up.
- **Using `import()` directly in CJS-compiled code**: TypeScript rewrites it to `require()`. Always use the `new Function('return import("@mdscript/mds")')` shim pattern for ESM-only deps in CJS context. Never pass an external string through the `new Function` boundary.
- **Forgetting to add a new package to both `verify-versions.mjs` and `bump-version.mjs`**: These two scripts have their own hardcoded `PKG_PATHS` arrays. Adding only one will cause the version gate to pass but bump-version to skip the new package (or vice versa).
- **Omitting `"repository": { "directory": "..." }` from a new package's `package.json`**: npm requires this field for provenance attestation on scoped packages (applies PF-003).

## Gotchas

- `cleanId()` must be called before passing an id to `shouldTransform()` and `transform()`. Vite appends `?t=xxx` cache-busting suffixes to ids. Forgetting `cleanId()` makes the compiled output have a query string embedded in its file path, which breaks `realpathSync()` and the dep tracking.
- `safeJsonForJs` escapes `<` to `<`. This is safe for JSON consumers but required to prevent `</script>` tag injection when the emitted module is inlined into an HTML `<script>` block.
- The `LazyInit` in `createMdsLoader` clears on rejection — a failing `esmImport()` will be retried on the next loader invocation. However, `capturedOptions` is only set when `lazy` is first created. If the first init fails and the second invocation has different options, the second options are used for init but the warning about option drift is NOT emitted (because `capturedOptions` is still null). This is an edge case of the retry behavior.
- `hmr-harness.mjs` uses `Date.now()` in the temp directory name (`mds-hmr-${process.pid}-${Date.now()}`). Parallel test runs on the same PID (unlikely but possible) could collide. The harness is designed for sequential execution.
- ADR-014 ordering in harness: `createTempMdsProject` writes files in insertion order. Callers **must** list `@import` dependency files before the entry file in the `files` object. Vite and Rollup watcher initialization depends on seeing deps registered before the entry is compiled.
- On macOS in development, `canon()` resolves `/tmp/...` to `/private/tmp/...` via `realpathSync`. Tests that hardcode `/tmp/` paths will see the canon form as `/private/tmp/` — this is intentional and correct. But tests that compare paths without `canon()` will see a mismatch.

## Key Files

- `packages/bundler-utils/src/transform.ts` — `createMdsTransformer`, emitted-module shape, `escapeForJs`, `safeJsonForJs`
- `packages/bundler-utils/src/loader.ts` — `createMdsLoader`, `new Function` CJS shim, `MdsLoaderApi` interface
- `packages/bundler-utils/src/frontmatter.ts` — `shouldTransform`, `cleanId`, `isMdsExtension`
- `packages/bundler-utils/src/lazy-init.ts` — `LazyInit<T>`: concurrent-safe dedup with generation-counter reset
- `packages/bundler-utils/src/types.ts` — canonical types: `MdsApi`, `CompileResult`, `TransformResult`, `MdsPluginOptions`, `FormattedError`
- `packages/vite-plugin/src/index.ts` — `mdsPlugin()`, `canon()`, `transformed` Set, `handleHotUpdate` G1 fix
- `packages/rollup-plugin/src/index.ts` — `mdsPlugin()` for Rollup, `addWatchFile` deps, `this.error()` Rollup error API
- `packages/webpack-loader/src/index.ts` — thin wrapper: `createMdsLoader()` at module scope
- `packages/rspack-loader/src/index.ts` — identical to webpack-loader wrapper
- `packages/bundler-utils/__test__/hmr-harness.mjs` — `HMR_ENABLED`, `createTempMdsProject`, `waitFor`, `waitForContent`, `editFile`
- `scripts/verify-versions.mjs` — 8-package version gate
- `scripts/bump-version.mjs` — 8-package coordinated bump
- `scripts/write-cjs-package.cjs` — writes `dist-cjs/package.json` `{"type":"commonjs"}`

## Related

- D1: Emitted module is HMR-runtime-free; webpack/rspack bubble to root (full reload); `hot:'only'` is a footgun — defines the no-self-accept contract.
- D3: `createMdsLoader()` factory in `bundler-utils` is the shared source for webpack-loader and rspack-loader; each call has independent state; webpack's `_setTransformerForTesting` is intentionally un-gated.
- D5: HMR watcher e2e specs are Linux-gated (`HMR_ENABLED = platform===linux || MDS_HMR=1`).
- D7: delete/recreate, create-after-error, `.md`→`type:mds` flip follow native bundler watcher semantics — documented limits, not parity hacks.
- ADR-014: `@import` dependency files must be written BEFORE the entry file in the HMR harness (deps-before-entry ordering).
- PF-003: `package.json` `repository.directory` is required for npm provenance on scoped packages.
- Vite G1 fix: closure-level `transformed` Set keyed by `canon()` (query/hash strip + `realpathSync` with `path.resolve` fallback), applied identically on insert and lookup, plus `ctx.modules` fallback in `handleHotUpdate`.
- Feature knowledge: `mds-js` (`packages/mds/`) — `@mdscript/mds` is the peer dep that `createMdsTransformer` calls `init()` and `compileFile()` on.
- Feature knowledge: `mds-cli` (`crates/mds-cli/src/watch.rs`) — `canon()` symlink resolution mirrors `watch.rs` `event_is_relevant` 3-layer matching.
- Feature knowledge: `mds-napi` (`crates/mds-napi/`) — the native backend that `@mdscript/mds` may load; `compileFile` and `init` shapes must remain stable.
