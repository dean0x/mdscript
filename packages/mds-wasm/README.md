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
// options.sourceMap — boolean; generate a Source Map v3 document.
//   For string-source compiles, sources[0] is "input.mds".
// options.sourcesContent — boolean; embed source text in map (requires sourceMap).
//   ⚠ Privacy: embeds the full template source.
// options.vars — { [key: string]: any } runtime variable overrides.
const result = compile(source, { sourceMap: true, vars: { name: 'World' } });
// result.sourceMap is a Source Map v3 object when sourceMap: true

// lint(source, options)
// options.vars — variable overrides.
// options.rules — { [ruleName: string]: 'off' | 'info' | 'warn' | 'error' }
const lintResult = lint(source, { rules: { 'shadow-variable': 'warn' } });
// lintResult: { version: 1, files: [...], truncated: boolean }
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
