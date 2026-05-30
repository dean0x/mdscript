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

Supported: `vite ^5.0.0 || ^6.0.0`.

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
// metadata.warnings     — string[]
// metadata.dependencies — string[] of imported file paths
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

## License

MIT
