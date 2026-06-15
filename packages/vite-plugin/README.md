# @mdscript/vite-plugin

Vite plugin for importing `.mds` templates as ES modules with HMR support.

> **Note:** This package is pre-release and not yet published to npm.

## Installation

```sh
npm install @mdscript/vite-plugin
```

## Peer dependencies

```sh
npm install @mdscript/mds vite
```

Supported: `vite ^5.0.0 || ^6.0.0 || ^7.0.0 || ^8.0.0`.

## Configuration

```ts
// vite.config.ts
import { defineConfig } from 'vite';
import mdsPlugin from '@mdscript/vite-plugin';

export default defineConfig({
  plugins: [
    mdsPlugin(),
    // or with options:
    mdsPlugin({ vars: { env: 'production' } }),
  ],
});
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

The plugin triggers a **full page reload** (not granular HMR) whenever an MDS-related
file changes. This is correct behaviour: MDS files export plain strings, not stateful
components, so there is nothing to hot-swap in place.

Files that trigger a reload:

- Any file with a `.mds` extension.
- Any `.md` file that has `type: mds` frontmatter (tracked after its first successful
  transform in the current dev-server session).
- Any file declared as an `@import` dependency of a compiled MDS file (transitive deps
  are tracked in the same session-level set).

### Known limitations

**AC-E1 — delete/recreate and new `@import` targets follow native Vite watch semantics.**
When you delete and recreate an `@import`-ed dependency file, or add an `@import` pointing
to a file that does not yet exist, recovery depends on Vite's own file-watching layer.
Touching a file that is already watched is usually enough to re-trigger the transform.
These are native Vite limits, not bugs in the plugin.

**AC-E2 — adding `type: mds` frontmatter to an existing `.md` file mid-session.**
If a `.md` file is loaded by Vite before you add `type: mds` frontmatter, it was never
transformed and is therefore not tracked. Subsequent edits to that file will not trigger a
reload until the dev server is restarted (or the file is re-transformed, e.g. by saving it
once after adding the frontmatter so Vite re-runs the transform hook).

**AC-E3 — error overlay points at compiled JS, not the `.mds` source.**
The plugin returns `map: null` (source maps are not yet generated). When a compile error
occurs, Vite's error overlay will point at the generated JavaScript position rather than the
original `.mds` source line. Use the error message text to locate the issue in your source
file.

## License

MIT
