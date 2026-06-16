# @mdscript/rollup-plugin

Rollup plugin for importing `.mds` templates as ES modules.

> **Note:** This package is pre-release and not yet published to npm.

## Installation

```sh
npm install @mdscript/rollup-plugin
```

## Peer dependencies

```sh
npm install @mdscript/mds rollup
```

Supported: `rollup ^3.0.0 || ^4.0.0`.

## Configuration

```js
// rollup.config.js
import mdsPlugin from '@mdscript/rollup-plugin';

export default {
  plugins: [
    mdsPlugin(),
    // or with options:
    mdsPlugin({ vars: { env: 'production' } }),
  ],
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

Rollup's plugin API does not expose an `handleHotUpdate` hook (that is Vite-specific).
In **watch mode** (`rollup --watch`), the plugin registers each `@import` dependency via
`this.addWatchFile()`. When any watched file changes, Rollup triggers a full rebuild of
the bundle — no runtime HMR injection is performed.

No `module.hot` or `import.meta.webpackHot` footer is appended to the emitted module.
The rebuild is Rollup's responsibility via its own file-watch graph.

### Known limitations

**AC-E1 — delete/recreate and new `@import` targets follow native Rollup watch semantics.**
Deleting and recreating an `@import`-ed dependency file, or adding an `@import` pointing
to a not-yet-created file, may require touching a watched file to recover. These are native
Rollup watch limits, not bugs in the plugin.

**AC-E2 — adding `type: mds` frontmatter to an existing `.md` file mid-watch.**
Rollup re-invokes `transform` on the next rebuild, so `shouldTransform` returning `true`
after frontmatter is added will compile the file correctly on the next watch cycle.

**AC-E3 — error overlay points at compiled JS, not the `.mds` source.**
The plugin returns `map: null`. Error positions refer to the generated JavaScript, not the
original `.mds` source line.

## License

MIT
