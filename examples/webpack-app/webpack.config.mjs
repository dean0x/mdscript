import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));

export default {
  mode: 'production',
  entry: './src/index.js',
  output: {
    filename: 'main.js',
    path: resolve(__dirname, 'dist'),
    library: { type: 'module' },
  },
  experiments: {
    outputModule: true,
  },
  module: {
    rules: [
      {
        test: /\.mds$/,
        use: {
          loader: '@mdscript/webpack-loader',
          options: {
            vars: { debug: false, mode: 'webpack-build' },
          },
        },
      },
    ],
  },
};
