# @mds/bundler-utils

Shared transform utilities for MDS bundler plugins (Vite, Rollup, Webpack).

> **Note:** This package is pre-release and not yet published to npm.

## Installation

```sh
npm install @mds/bundler-utils
```

## Peer dependencies

```sh
npm install @mds/mds
```

## Usage

This package is primarily consumed by the bundler-specific plugin packages
(`@mds/vite-plugin`, `@mds/rollup-plugin`, `@mds/webpack-loader`). You only
need to use it directly if you are writing a plugin for another bundler.

```ts
import { createMdsTransformer, formatMdsError, shouldTransform } from '@mds/bundler-utils';

// Lazily initialize (call once per build, after loading @mds/mds)
const mds = await import('@mds/mds');
const transformer = createMdsTransformer(mds, { vars: { env: 'production' } });

// Transform a .mds file to a JavaScript module
if (await transformer.shouldTransform('/path/to/file.mds')) {
  const result = await transformer.transform('/path/to/file.mds');
  // result.code        — JS module source
  // result.dependencies — absolute paths of transitively imported files
  // result.warnings    — non-fatal compiler warnings
}
```

## TypeScript module declarations

To tell TypeScript about `.mds` imports, add the following to your `tsconfig.json`:

```json
{
  "compilerOptions": {
    "types": ["@mds/bundler-utils/mds"]
  }
}
```

Or add a triple-slash reference in any `.d.ts` file in your project:

```ts
/// <reference types="@mds/bundler-utils/mds" />
```

This makes `import content from './prompt.mds'` type-safe: `content` is `string`
and the module also exports `metadata: { warnings: string[]; dependencies: string[] }`.

## Options

```ts
interface MdsPluginOptions {
  /** Variables available for interpolation in .mds templates. */
  vars?: Record<string, unknown>;
}
```

## License

MIT
