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
