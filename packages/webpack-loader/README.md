# @mdscript/webpack-loader

Webpack 5 loader for importing `.mds` templates as ES modules.

> **Note:** This package is pre-release and not yet published to npm.

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
