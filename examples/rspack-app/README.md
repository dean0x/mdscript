# examples/rspack-app

Demonstrates importing `.mds` templates through `@mdscript/rspack-loader` in an
[Rspack](https://rspack.dev) project.

## Setup

From the repo root, build the workspace packages first:

```bash
npm install && npm run build --workspaces --if-present
```

Then install this example's dependencies and build:

```bash
cd examples/rspack-app
npm install
npm run build
```

The compiled bundle lands in `dist/main.js`.

## How it works

`rspack.config.mjs` configures the MDS loader for `.mds` files:

```js
module: {
  rules: [
    {
      test: /\.mds$/,
      use: {
        loader: '@mdscript/rspack-loader',
        options: { vars: { debug: false, mode: 'rspack-build' } },
      },
    },
  ],
},
```

Each `.mds` import resolves to the compiled Markdown string (plus a `metadata` export).
The build uses rspack's programmatic JS API via `build.mjs` — no `@rspack/cli` needed.
