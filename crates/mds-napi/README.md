# @mdscript/mds-napi

Native Node.js bindings for the [MDS (Markdown Script)](https://github.com/dean0x/mdscript)
compiler, built with [napi-rs](https://napi.rs/).

This is the high-performance backend used by [`@mdscript/mds`](https://www.npmjs.com/package/@mdscript/mds)
on Node.js. **Most users should depend on `@mdscript/mds`, not this package
directly** — `@mdscript/mds` loads this native addon automatically and falls back
to [`@mdscript/mds-wasm`](https://www.npmjs.com/package/@mdscript/mds-wasm) when a
prebuilt binary is unavailable.

## How it loads

This host package contains only the loader (`index.js`) and TypeScript types
(`index.d.ts`). The compiled `.node` binaries ship in per-platform packages
declared as `optionalDependencies`, filtered by `os`/`cpu`/`libc`:

| Platform package | Target |
|------------------|--------|
| `@mdscript/mds-napi-darwin-arm64` | macOS Apple Silicon |
| `@mdscript/mds-napi-darwin-x64` | macOS Intel |
| `@mdscript/mds-napi-linux-x64-gnu` | Linux x64 (glibc) |
| `@mdscript/mds-napi-linux-x64-musl` | Linux x64 (musl) |
| `@mdscript/mds-napi-linux-arm64-gnu` | Linux arm64 (glibc) |
| `@mdscript/mds-napi-linux-arm64-musl` | Linux arm64 (musl) |
| `@mdscript/mds-napi-win32-x64-msvc` | Windows x64 |

`index.js` selects the matching binary at runtime from `process.platform`,
`process.arch`, and (on Linux) the detected libc.

## API

```js
const { compile, compileFile, check, checkFile, lint, lintFile, lintVirtual } = require('@mdscript/mds-napi');
```

### `compile(source, opts?)`

Compile an MDS source string. Returns a discriminated-union result object:

- Markdown: `{ kind: "markdown", output: string, warnings: string[], dependencies: string[], sourceMap?: object }`
- Messages: `{ kind: "messages", messages: [{role,content},...], warnings: string[], dependencies: string[] }`

Options:
- `basePath` (string) — base directory for `@import` resolution; defaults to cwd.
- `vars` (object) — runtime variable overrides.
- `sourceMap` (boolean) — generate a Source Map v3 document; result gains `sourceMap`.
  For string-source compiles `sources[0]` is `"input.mds"`.
- `sourcesContent` (boolean) — embed original source text in the map (requires `sourceMap`).
  ⚠ Privacy: embeds the full template source.

### `compileFile(path, opts?)`

Same result shape as `compile`. Options: `vars`, `sourceMap`, `sourcesContent`.
`basePath` is not accepted — the base directory is derived from the file path.

### `check(source, opts?)` / `checkFile(path, opts?)`

Validate without rendering. Returns `{ warnings: string[] }`.
Options: `basePath`, `vars` (check only; checkFile: `vars` only).
Source-map options are **not accepted** — check does not generate output.

### `lint(source, opts?)` / `lintFile(path, opts?)` / `lintVirtual(modules, entry, opts?)`

Static analysis. Returns the canonical lint JSON:
`{ version: 1, files: [{file, diagnostics: [{rule, severity, message, help, fixable, span?},...]},...], truncated: bool }`

Options: `basePath` (lint/lintVirtual only), `vars`, `rules` (`Record<string, "off"|"info"|"warn"|"error">`).

See `index.d.ts` for the full typed surface.

## License

MIT — see [LICENSE](./LICENSE).
