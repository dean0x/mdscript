# @mdscript/rspack-loader

Rspack loader for importing MDS templates as ES modules.

## Installation

```bash
npm install @mdscript/rspack-loader @mdscript/mds
```

## Usage

```js
// rspack.config.js
module.exports = {
  module: {
    rules: [
      {
        test: /\.mds$/,
        use: ['@mdscript/rspack-loader'],
      },
    ],
  },
};
```

Importing an `.mds` file yields two exports:

```js
import template, { metadata } from './prompt.mds';
// template  — compiled string output
// metadata  — { warnings: string[], dependencies: string[] }
```

<!-- HMR notes added in Step 8 -->

## License

MIT
