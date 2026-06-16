# @mdscript/rspack-loader

Rspack loader for importing MDS templates as ES modules.

## Installation

```bash
npm install @mdscript/rspack-loader @mdscript/mds
```

## Peer dependencies

```sh
npm install @mdscript/mds @rspack/core
```

Supported: `@rspack/core ^1.0.0`.

## Configuration

```js
// rspack.config.js
export default {
  module: {
    rules: [
      {
        test: /\.mds$/,
        use: {
          loader: '@mdscript/rspack-loader',
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

When running rspack's dev server with `hot: true` (the default), changes to `.mds` files
participate in HMR via rspack's module graph. Because the emitted module has **no
`import.meta.webpackHot` self-accept footer**, rspack bubbles the HMR event up to the root
entry. The result is a **full page reload** whenever an `.mds` file or any of its `@import`
dependencies changes. This is correct behaviour: MDS files export plain strings, not
stateful components.

> **`hot: 'only'` is a footgun.** If you set `devServer: { hot: 'only' }`, rspack will
> suppress the full page reload rather than falling back to it. The compiled-string change
> will not appear without a manual browser refresh. Leave `hot: true` (the default).

No `import.meta.webpackHot` footer is injected into the emitted module.
HMR event propagation is rspack's responsibility via its module graph and `addDependency()`
calls made by the loader. rspack 1.x uses the same HMR API shape as webpack 5.

### Known limitations

**AC-E1 — delete/recreate and new `@import` targets follow native rspack watch semantics.**
Deleting and recreating an `@import`-ed dependency file, or adding an `@import` pointing
to a not-yet-created file, may require touching a watched file to prompt rspack to
re-resolve the dependency graph. These are native rspack limits, not bugs in the loader.

**AC-E2 — adding `type: mds` frontmatter to an existing `.md` file mid-session.**
rspack re-invokes the loader on rebuild, so `shouldTransform` returning `true` after
frontmatter is added will compile the file correctly on the next rebuild cycle.

**AC-E3 — error overlay points at compiled JS, not the `.mds` source.**
The loader does not emit source maps (`map: null`). rspack's error overlay will point at
the generated JavaScript position rather than the original `.mds` source line. Use the
error message text to locate the issue in your source file.

## License

MIT
