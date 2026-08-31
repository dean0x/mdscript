# MDS × Rollup example

Demonstrates [`@mdscript/rollup-plugin`](../../packages/rollup-plugin) compiling
`.mds` prompt templates into ES modules in a plain Rollup build.

## Build

```bash
npm install
npm run build      # rollup -c → dist/main.mjs
```

`dist/main.mjs` contains the compiled prompts as exported strings, and each
`.mds` module also exports `metadata: { warnings, dependencies }` —
`metadata.dependencies` entries are project-root-relative POSIX paths, never
absolute host paths.

## Configuration notes

`rollup.config.mjs` wires two plugins, and the `nodeResolve` `extensions`
option is required — the entry point is TypeScript, and without
`extensions: ['.ts', '.js']` Rollup cannot resolve the `.ts` import graph:

```js
plugins: [
  mdsPlugin({ vars: { debug: false, mode: 'rollup-build' } }),
  nodeResolve({ extensions: ['.ts', '.js'] }),
],
```

## Watch mode

```bash
npx rollup -c -w
```

Editing an `.mds` template — or any file it transitively `@import`s — triggers
a rebuild: the plugin registers every compiled dependency via `addWatchFile`.
