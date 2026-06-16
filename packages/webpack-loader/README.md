# @mdscript/webpack-loader

Webpack 5 loader for importing `.mds` templates as ES modules.

## Installation

```sh
npm install @mdscript/webpack-loader
```

## Peer dependencies

```sh
npm install @mdscript/mds webpack
```

Supported: `webpack ^5.0.0`.

## Configuration

```js
// webpack.config.js
export default {
  module: {
    rules: [
      {
        test: /\.mds$/,
        use: {
          loader: '@mdscript/webpack-loader',
          options: {
            // optional
            vars: { env: 'production' },
          },
        },
      },
    ],
  },
};
```

## Usage

```ts
import content from './system-prompt.mds';
// content is a compiled Markdown string

import content, { metadata } from './system-prompt.mds';
// metadata.warnings     - string[]
// metadata.dependencies - string[] of imported file paths
```

## TypeScript setup

Add the module declaration to your `tsconfig.json` so TypeScript recognises
`.mds` imports:

```json
{
  "compilerOptions": {
    "types": ["@mdscript/bundler-utils/mds"]
  }
}
```

## Options

```ts
interface MdsPluginOptions {
  /** Variables available for interpolation in .mds templates. */
  vars?: Record<string, unknown>;
}
```

## HMR / dev server

When running webpack-dev-server with `hot: true` (the default), changes to `.mds` files
participate in HMR via webpack's module graph. Because the emitted module has **no
`module.hot.accept()` self-accept footer**, webpack bubbles the HMR event up to the root
entry. The result is a **full page reload** whenever an `.mds` file or any of its `@import`
dependencies changes. This is correct behaviour: MDS files export plain strings, not
stateful components.

> **`hot: 'only'` is a footgun.** If you set `devServer: { hot: 'only' }`, webpack will
> suppress the full page reload rather than falling back to it. The compiled-string change
> will not appear without a manual browser refresh. Leave `hot: true` (the default).

No `module.hot.accept()` footer is injected into the emitted module.
HMR event propagation is webpack's responsibility via its module graph and `addDependency()`
calls made by the loader.

### Known limitations

**AC-E1 — delete/recreate and new `@import` targets follow native webpack watch semantics.**
Deleting and recreating an `@import`-ed dependency file, or adding an `@import` pointing
to a not-yet-created file, may require touching a watched file to prompt webpack to
re-resolve the dependency graph. These are native webpack limits, not bugs in the loader.

**AC-E2 — adding `type: mds` frontmatter to an existing `.md` file mid-session.**
webpack re-invokes the loader on rebuild, so `shouldTransform` returning `true` after
frontmatter is added will compile the file correctly on the next rebuild cycle.

**AC-E3 — error overlay points at compiled JS, not the `.mds` source.**
The loader does not emit source maps (`map: null`). webpack's error overlay will point at
the generated JavaScript position rather than the original `.mds` source line. Use the
error message text to locate the issue in your source file.

## License

MIT
