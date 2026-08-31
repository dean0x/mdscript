# examples/webpack-app

Demonstrates importing `.mds` templates through `@mdscript/webpack-loader` in a
[webpack](https://webpack.js.org) project.

## Setup

From the repo root, build the workspace packages first:

```bash
npm install && npm run build --workspaces --if-present
```

Then install this example's dependencies and build:

```bash
cd examples/webpack-app
npm install
npm run build
```

The compiled bundle lands in `dist/main.js`.

## How it works

`webpack.config.mjs` configures the MDS loader for `.mds` files:

```js
module: {
  rules: [
    {
      test: /\.mds$/,
      use: {
        loader: '@mdscript/webpack-loader',
        options: { vars: { debug: false, mode: 'webpack-build' } },
      },
    },
  ],
},
```

Each `.mds` import resolves to the compiled Markdown string (plus a `metadata`
export). The config sets `experiments.outputModule: true` with
`library: { type: 'module' }` so the bundle is emitted as an ES module.

In `webpack --watch`, editing an `.mds` template — or any file it transitively
`@import`s — triggers a rebuild: the loader registers every compiled dependency
via `this.addDependency`.
