---
feature: bundler-plugins
name: Bundler Plugins (bundler-utils + Vite/Rollup/Webpack/Rspack)
description: "Use when adding a new bundler integration, modifying the emitted-module contract, debugging HMR behavior, working on the CJS compatibility shim, updating the transformer/loader factory, registering a new package in the release pipeline, or investigating why a .mds file emits unexpected output. Keywords: createMdsTransformer, createMdsLoader, bundler-utils, vite-plugin, rollup-plugin, webpack-loader, rspack-loader, addWatchFile, addDependency, handleHotUpdate, emitted module contract, export default string, export default Message[], safeJsonForJs, escapeForJs, metadata, kind, markdown, messages, discriminated union, mds.d.ts, MdsMessage, string | MdsMessage[]."
category: component-patterns
directories: ["packages/bundler-utils/", "packages/vite-plugin/", "packages/rollup-plugin/", "packages/webpack-loader/", "packages/rspack-loader/"]
referencedFiles:
  - packages/bundler-utils/src/transform.ts
  - packages/bundler-utils/src/types.ts
  - packages/bundler-utils/src/loader.ts
  - packages/bundler-utils/src/frontmatter.ts
  - packages/bundler-utils/src/lazy-init.ts
  - packages/bundler-utils/mds.d.ts
  - packages/bundler-utils/src/index.ts
  - packages/vite-plugin/src/index.ts
  - packages/rollup-plugin/src/index.ts
  - packages/webpack-loader/src/index.ts
  - packages/rspack-loader/src/index.ts
created: 2026-06-26
updated: 2026-06-26
---

# Bundler Plugins (bundler-utils + Vite/Rollup/Webpack/Rspack)

## Overview

`packages/bundler-utils/` is the shared transformation layer consumed by four bundler plugins: `vite-plugin`, `rollup-plugin`, `webpack-loader`, `rspack-loader`. It implements `createMdsTransformer` (used by Vite/Rollup) and `createMdsLoader` (used by Webpack/Rspack). After the intrinsic-output refactor, the emitted JS module branches on the compiled `kind` — a markdown `.mds` emits a string default export, a messages `.mds` emits a `Message[]` default export. The published `mds.d.ts` ambient declaration reflects this widened type.

## Core Responsibilities

- `transform.ts`: compile `.mds` files via `MdsApi.compileFile`, emit the JS module source (`export default`), serialize metadata
- `loader.ts`: webpack/rspack integration via `createMdsLoader`
- `frontmatter.ts`: `shouldTransform(id)` — decides if a module ID refers to an `.mds` file
- `lazy-init.ts`: `LazyInit<T>` — ensures `mds.init()` is awaited exactly once per transformer instance
- Does NOT: implement compilation logic, manage caching, handle HMR (delegated to plugin wrappers)

## Standard Structure

### Emitted module contract (post-refactor)

The emitted JS module branches on `result.kind`:

```typescript
// transform.ts — inside transform()
let defaultExport: string;
if (result.kind === 'markdown') {
  // Escape the string for embedding in a double-quoted JS literal
  defaultExport = `export default "${escapeForJs(result.output)}";\n`;
} else {
  // kind === 'messages' — serialize the messages array as safe inline JSON
  defaultExport = `export default ${safeJsonForJs(result.messages)};\n`;
}

const code =
  defaultExport +
  `export const metadata = ${safeJsonForJs({ warnings: result.warnings, dependencies: result.dependencies })};\n`;
```

So for a markdown `.mds`: `export default "..."` (string)
For a messages `.mds`: `export default [{role:"...", content:"..."}]` (array literal)

Both emit `export const metadata = { warnings: [...], dependencies: [...] };`

### safeJsonForJs vs escapeForJs

These two serializers have different contracts and must not be swapped:

- `escapeForJs(str: string): string` — escapes special chars for embedding inside a double-quoted JS string literal (`"..."`)
- `safeJsonForJs(value: unknown): string` — `JSON.stringify` + escapes `<`, U+2028, U+2029 for safe inline `<script>` embedding; used for array/object literals in `export default`

```typescript
// escapeForJs handles: \, ", \n, \r, \0, U+2028, U+2029
export default "${escapeForJs(result.output)}"  // for strings

// safeJsonForJs handles: <, U+2028, U+2029 (JSON.stringify handles the rest)
export default ${safeJsonForJs(result.messages)} // for objects/arrays
```

`safeJsonForJs` is exported from `transform.ts` so tests can verify escape behavior directly.

### U+2028/U+2029 construction pattern

Literal U+2028 (line separator) and U+2029 (paragraph separator) cannot appear in regex literals or object key literals — the JS parser treats them as line terminators. Always use:

```typescript
// Regex: use new RegExp() string
const JS_ESCAPE_RE = new RegExp('[\\\\\"\\n\\r\\0\\u2028\\u2029]', 'g');

// Map keys: computed property assignment after the literal
JS_ESCAPE_MAP[String.fromCodePoint(0x2028)] = '\\u2028';
JS_ESCAPE_MAP[String.fromCodePoint(0x2029)] = '\\u2029';
```

### Published type declaration (mds.d.ts)

```typescript
// packages/bundler-utils/mds.d.ts
interface MdsMessage { role: string; content: string; }

declare module '*.mds' {
  // Widened to union: string for markdown, MdsMessage[] for messages
  const content: string | MdsMessage[];
  export default content;
  export const metadata: { warnings: string[]; dependencies: string[] };
}
```

TypeScript consumers must narrow on `Array.isArray(content)` or similar to distinguish the two variants. The declaration uses a local `MdsMessage` interface (not importing from `@mdscript/mds`) to avoid a dependency cycle.

### MdsApi interface (bundler-utils/src/types.ts)

```typescript
export interface MdsApi {
  compileFile(path: string, options?: { vars?: Record<string, unknown> }): Promise<CompileResult>;
  init(): Promise<void>;
}

export type CompileResult = MarkdownResult | MessagesResult;

export interface MarkdownResult {
  kind: 'markdown'; output: string; warnings: string[]; dependencies: string[];
}
export interface MessagesResult {
  kind: 'messages'; messages: Message[]; warnings: string[]; dependencies: string[];
}
```

The `MdsApi` interface intentionally omits `InitOptions` (bundler plugins always call `init()` with no arguments).

### createMdsTransformer

```typescript
// Returns: { shouldTransform(id): boolean, transform(id): Promise<TransformResult> }
export function createMdsTransformer(mds: MdsApi, options?: MdsPluginOptions): { ... }
```

Stateful: `LazyInit<void>` ensures `mds.init()` is awaited once. The `id` passed to `transform(id)` is trusted (sourced from bundler module pipeline); query/hash stripping is the plugin's responsibility.

## Dependency Patterns

Bundler plugin factories (`vite-plugin`, etc.) import `createMdsTransformer` or `createMdsLoader` from `bundler-utils` and pass in the `@mdscript/mds` module import. The `MdsApi` structural type means the real package satisfies it without an `implements` declaration.

Test mocks for `MdsApi.compileFile` must return the new discriminated-union shape:

```javascript
// Correct mock for transform tests
compileFile: async (id) => ({ kind: 'markdown', output: '# Test', warnings: [], dependencies: [] })
// or
compileFile: async (id) => ({ kind: 'messages', messages: [{role:'user',content:'hi'}], warnings: [], dependencies: [] })
```

Old mocks returning `{ output: ..., warnings: [], deps: [] }` (flat shape) are wrong — update them.

## Error Handling

Compilation errors from `mds.compileFile()` propagate through `transform()` as thrown exceptions. The bundler plugin wrappers (Vite, Rollup, Webpack) catch them and format them into bundler-specific error reporting via `FormattedError`.

## Anti-Patterns

- **Using `escapeForJs` for objects/arrays** — only valid for string values inside `"..."`. Use `safeJsonForJs` for `export default [{...}]`.
- **Using `safeJsonForJs` for string default exports** — it won't add JS string literal quotes; use `escapeForJs` and wrap in `"..."`.
- **Using literal U+2028/U+2029 in regex or map keys** — JS parser treats them as line terminators; use `new RegExp()` and `String.fromCodePoint()`.
- **Importing `Message` type from `@mdscript/mds` in `mds.d.ts`** — creates a dependency cycle; use the local `MdsMessage` interface.
- **Emitting `export default null` or `export default undefined` for messages** — the inactive case must be omitted at the napi level; the transformer always has a real messages array.
- **Calling `mds.init()` unconditionally on every transform** — `LazyInit` ensures it's called once; re-calling it is wasteful and may break WASM backends.

## Gotchas

- `metadata` is emitted as a named export on BOTH kinds. Consumers that only use `metadata.dependencies` for HMR registration don't need to branch on kind.
- The `shouldTransform(id)` function (`frontmatter.ts`) checks the file extension; it does NOT read file content. A `.mds` file with type-mds frontmatter is handled by `shouldTransform` returning true for the extension alone.
- Watch mode: `addWatchFile` / `addDependency` are called with `result.dependencies` from the transform result, regardless of kind. HMR works identically for markdown and messages mode.
- `LazyInit` stores state per transformer instance. Each call to `createMdsTransformer` creates an independent lazy initializer — `D3 createMdsLoader factory independent state`.
- Linux HMR e2e tests are gated behind `MDS_HMR=1` env var; they are timing-sensitive and may flake on re-run.

## Key Files

- `packages/bundler-utils/src/transform.ts` — `createMdsTransformer`, `safeJsonForJs`, `escapeForJs`, kind-branching emit logic
- `packages/bundler-utils/src/types.ts` — `MdsApi`, `CompileResult`, `MarkdownResult`, `MessagesResult`, `Message`, `TransformResult`
- `packages/bundler-utils/mds.d.ts` — ambient `*.mds` module declaration; `string | MdsMessage[]` default export type
- `packages/bundler-utils/src/loader.ts` — `createMdsLoader` (Webpack/Rspack variant)
- `packages/bundler-utils/src/index.ts` — re-exports `MarkdownResult`, `MessagesResult`, `Message` from types

## Related

- Feature: mds-js — the `@mdscript/mds` package that implements `MdsApi`; `CompileResult` union defined in `packages/mds/src/types.ts` mirrors the `MdsApi.compileFile` return type
- Feature: mds-napi — the native backend that produces the discriminated-union result consumed via `MdsApi`
- Feature: mds-compiler — `CompiledOutput::Messages(Vec<Message>)` is the Rust type that drives messages mode; its wire format shapes the JS `MessagesResult`
