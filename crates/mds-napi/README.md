# @mdscript/mds-napi

Native Node.js bindings for the [MDS (Markdown Script)](https://github.com/dean0x/mds)
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
const { compile, check, compileFile, checkFile } = require('@mdscript/mds-napi');
```

See `index.d.ts` for the full typed surface.

## License

MIT — see [LICENSE](./LICENSE).
