# @mdscript/mds-wasm

WebAssembly build of the [MDS (Markdown Script)](https://github.com/dean0x/mdscript)
compiler.

This package is the portable fallback used by [`@mdscript/mds`](https://www.npmjs.com/package/@mdscript/mds)
when the native addon (`@mdscript/mds-napi`) is unavailable, and it powers the
browser build. **Most users should depend on `@mdscript/mds`, not this package
directly**. `@mdscript/mds` selects the native addon on Node and this WASM build
on the web (or as a Node fallback) automatically.

## What's inside

Two builds, selected by package `exports` conditions:

| Condition | Entry | Module type | Init |
|-----------|-------|-------------|------|
| `node` | `dist/node/mds_wasm.js` | CommonJS (`wasm-pack --target nodejs`) | none |
| `browser` / `default` | `dist/web/mds_wasm.js` | ESM (`wasm-pack --target web`) | call `default()` with the `.wasm` URL |

Each build exposes `compile(source, options)`, `check(source, options)`,
`lint(source, options)`, `lintVirtual(modules, entry, options)`, and `scanImports(source)`.

### Options

```js
// compile(source, options)
// options.filename — string (default "input.mds"): key used for this source in the
//   virtual FS and as sources[0] in the generated source map. Override when you want
//   a meaningful name to appear in source maps or import paths.
// options.modules — { [key: string]: string }: additional virtual modules for
//   @import resolution. The entry source is inserted under options.filename.
// options.vars — { [key: string]: any }: runtime variable overrides.
// options.sourceMap — boolean: generate a Source Map v3 document; result gains .sourceMap.
//   sources[0] is options.filename (default "input.mds").
// options.sourcesContent — boolean: embed original source text in sourcesContent[]
//   (requires sourceMap: true). ⚠ Privacy: embeds the full template source.
const result = compile(source, { sourceMap: true, vars: { name: 'World' } });
// result.sourceMap is a Source Map v3 object when sourceMap: true

// check(source, options)
// Accepted keys: filename, modules, vars. (sourceMap/sourcesContent are parsed but
// not applied — check does not generate output. Use compile for source maps.)
const checked = check(source, { vars: { name: 'World' } });
// returns { warnings: string[] }

// lint(source, options)
// Accepted keys: filename, modules, vars, rules.
// options.rules — { [ruleName: string]: 'off' | 'info' | 'warn' | 'error' }
//   Unknown rule names emit a warning and lint continues — the unknown name has no effect
//   (the rule is not enforced); unknown severity values throw.
//   When unknown rule names are present, lintResult.lint_warnings is a non-empty string[].
const lintResult = lint(source, { rules: { 'shadow-variable': 'warn' } });
// lintResult: { version: 1, files: [...], truncated: boolean, lint_warnings?: string[] }

// lintVirtual(modules, entry, options)
// modules: { [key: string]: string } — the full virtual module map.
// entry: string — key of the entry module within modules.
// Accepted option keys: vars, rules. (filename and modules are top-level args, not options.)
const vResult = lintVirtual({ 'main.mds': source }, 'main.mds', { rules: {} });
```

## Build

```bash
npm run build -w @mdscript/mds-wasm
```

Requires [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) and the
`wasm32-unknown-unknown` Rust target. Output is written to `dist/node` and
`dist/web`.

## License

MIT. See [LICENSE](./LICENSE).
